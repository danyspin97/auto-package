//! Running a checkout's `_service` inside bubblewrap, with the network
//! limited to the source hosts.
//!
//! The services (`tar_scm`, `cargo_vendor`, `go_modules`, ...) fetch and
//! unpack attacker-influenced sources and run build tooling on them. They run
//! here instead of through `osc service ra`, which needs the OBS credentials
//! (it asks the API for the project-wide services, and osc reads the
//! credentials from the desktop keyring over the session bus). Each service is
//! the `/usr/lib/obs/service/<name>` helper, called the way osc calls it, with:
//!
//! - only the checkout bound read-write (its `.osc` read-only), the system
//!   read-only, a private `/tmp` and an empty `HOME`, no session bus, no oscrc;
//! - every namespace unshared, the network included: the only way out is a
//!   loopback listener inside the sandbox (a small Python bridge) that hands
//!   each connection to a unix socket served by [`Proxy`], a CONNECT-only
//!   proxy that allows port 443 on the allowlisted hosts and refuses private
//!   and local addresses. A tool that ignores the proxy variables gets no
//!   network at all.
//!
//! Without a working bubblewrap the services are refused, unless
//! `AUTO_PACKAGE_UNSAFE_SERVICE=1` asks for the old unsandboxed `osc service ra`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::discovery::{TRUSTED_SOURCE_HOSTS, extra_source_hosts, host_is_under};
use crate::osc::Error;
use crate::proc::{self, Captured, Emit};

/// Hosts the services fetch from, besides the trusted source hosts of
/// [`crate::discovery`], the fixed forges and `AUTO_PACKAGE_SOURCE_HOSTS`.
/// Subdomains of an entry are accepted too, as there.
pub(crate) const SERVICE_HOSTS: &[&str] = &[
    "github.com",
    "codeload.github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    // GitHub release downloads redirect here since 2025.
    "release-assets.githubusercontent.com",
    "gitlab.com",
    "gitlab.freedesktop.org",
    "gitlab.gnome.org",
    "salsa.debian.org",
    "bitbucket.org",
    "sourceforge.net",
    "downloads.sourceforge.net",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "registry.npmjs.org",
    "proxy.golang.org",
    "sum.golang.org",
];

/// The switch that runs services unsandboxed, as before the sandbox.
pub(crate) const UNSAFE_ENV: &str = "AUTO_PACKAGE_UNSAFE_SERVICE";
/// Where osc finds the service helpers.
const SERVICE_DIR: &str = "/usr/lib/obs/service";
/// The interpreter running the loopback bridge; osc itself needs it.
const PYTHON: &str = "/usr/bin/python3";
/// Where the proxy's socket directory appears inside the sandbox.
const SANDBOX_PROXY_DIR: &str = "/run/ap-proxy";
const SOCKET_NAME: &str = "proxy.sock";
/// The bridge's port on the sandbox's own loopback interface.
const PROXY_PORT: u16 = 3128;
/// The empty home directory inside the sandbox (on its private `/tmp`).
const SANDBOX_HOME: &str = "/tmp/home";
/// Any non-empty value: tar_scm clones into the working directory under osc.
const OSC_VERSION: &str = "1.0.0";

// ── the allowlist ───────────────────────────────────────────────────────────

/// `raw` as a host name to compare: lowercase, without one trailing dot.
/// IP literals, numeric forms a resolver may read as an address, non-ASCII
/// (unencoded IDN) names and malformed labels are refused.
pub(crate) fn normalize_host(raw: &str) -> Result<String, String> {
    if !raw.is_ascii() {
        return Err(format!("host `{raw}` is not ASCII"));
    }
    let lower = raw.to_ascii_lowercase();
    let host = lower.strip_suffix('.').unwrap_or(&lower);
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err(format!("host `{raw}` is an IP address"));
    }
    if host.is_empty() || host.len() > 253 {
        return Err(format!("host `{raw}` is not a valid name"));
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("host `{raw}` is not a valid name"));
        }
    }
    // No real name ends in a numeric label (`2130706433`, `0x7f.1`).
    let last = host.rsplit('.').next().unwrap_or("");
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return Err(format!("host `{raw}` is an IP address"));
    }
    Ok(host.to_string())
}

/// Whether the normalized `host` is a service host, a trusted source host, a
/// fixed forge, or under one of `extra`.
pub(crate) fn host_allowed_in(host: &str, extra: &[String]) -> bool {
    SERVICE_HOSTS
        .iter()
        .chain(TRUSTED_SOURCE_HOSTS)
        .chain(ap_ecosystem::forge::FIXED_FORGE_HOSTS)
        .any(|d| host_is_under(host, d))
        || extra.iter().any(|d| host_is_under(host, d))
}

/// Whether a service may connect to `host:port`: port 443 on an allowed host
/// only. Returns the normalized host.
pub(crate) fn check_target(host: &str, port: u16, extra: &[String]) -> Result<String, String> {
    if port != 443 {
        return Err(format!("port {port} is not allowed, only 443"));
    }
    let host = normalize_host(host)?;
    if !host_allowed_in(&host, extra) {
        return Err(format!(
            "{host} is not an allowed source host; set AUTO_PACKAGE_SOURCE_HOSTS={host} to allow it"
        ));
    }
    Ok(host)
}

/// `host[:port]` split; `default` is the port when none is given (`None`:
/// a port is required). Bracketed IPv6 literals are refused.
fn split_host_port(authority: &str, default: Option<u16>) -> Result<(&str, u16), String> {
    if authority.starts_with('[') {
        return Err(format!("`{authority}` is an IP address"));
    }
    match (authority.rsplit_once(':'), default) {
        (Some((host, port)), _) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("`{authority}` has a bad port"))?;
            Ok((host, port))
        }
        (None, Some(port)) => Ok((authority, port)),
        (None, None) => Err(format!("`{authority}` has no port")),
    }
}

/// Check a `url` service parameter: https only, no user info, an allowed host
/// on port 443.
pub(crate) fn check_service_url(url: &str) -> Result<(), String> {
    check_service_url_in(url, &extra_source_hosts())
}

fn check_service_url_in(url: &str, extra: &[String]) -> Result<(), String> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("url `{url}` contains whitespace"));
    }
    let lower = url.to_ascii_lowercase();
    if let Some((scheme, _)) = lower.split_once("://")
        && scheme != "https"
    {
        return Err(format!("url `{url}` uses {scheme}://, it must be https://"));
    }
    if !lower.starts_with("https://") {
        return Err(format!("url `{url}` must be https://"));
    }
    let rest = &url["https://".len()..];
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(format!("url `{url}` carries user info"));
    }
    let (host, port) =
        split_host_port(authority, Some(443)).map_err(|e| format!("url `{url}`: {e}"))?;
    check_target(host, port, extra)
        .map(|_| ())
        .map_err(|e| format!("url `{url}`: {e}"))
}

// ── the proxy ───────────────────────────────────────────────────────────────

/// Longest request head the proxy reads.
const MAX_HEAD: usize = 8 * 1024;
/// Most header lines after the request line.
const MAX_HEADER_LINES: usize = 64;
/// Time a client has to send its request.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Why the proxy answers a request with an error.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub status: u16,
    pub reason: String,
}

fn refuse(status: u16, reason: impl Into<String>) -> Refusal {
    Refusal {
        status,
        reason: reason.into(),
    }
}

/// Read a request head (up to the blank line). Returns it and whatever the
/// client sent after it.
pub(crate) fn read_head(r: &mut impl Read) -> Result<(String, Vec<u8>), Refusal> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if pos + 4 > MAX_HEAD {
                return Err(refuse(431, "request head too long"));
            }
            let rest = buf.split_off(pos + 4);
            let head = String::from_utf8(buf).map_err(|_| refuse(400, "request is not UTF-8"))?;
            return Ok((head, rest));
        }
        if buf.len() >= MAX_HEAD {
            return Err(refuse(431, "request head too long"));
        }
        let n = match r.read(&mut chunk) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(refuse(408, "request not received in time"));
            }
            Err(e) => return Err(refuse(400, format!("cannot read the request: {e}"))),
        };
        if n == 0 {
            return Err(refuse(400, "connection closed before the request ended"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The `host` and `port` of a `CONNECT host:port HTTP/1.x` request head.
/// Anything else is refused.
pub(crate) fn parse_connect(head: &str) -> Result<(String, u16), Refusal> {
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("");
    if lines.filter(|l| !l.is_empty()).count() > MAX_HEADER_LINES {
        return Err(refuse(431, "too many header lines"));
    }
    let parts: Vec<&str> = request.split(' ').collect();
    let [method, target, version] = parts[..] else {
        return Err(refuse(400, "malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(refuse(400, "not an HTTP/1 request"));
    }
    if method != "CONNECT" {
        return Err(refuse(
            405,
            format!("{method} is not supported, only CONNECT"),
        ));
    }
    if target.contains('@') {
        return Err(refuse(403, format!("{target}: user info is not allowed")));
    }
    if target.contains('/') {
        return Err(refuse(400, format!("{target} is not host:port")));
    }
    let (host, port) = split_host_port(target, None).map_err(|e| refuse(400, e))?;
    Ok((host.to_string(), port))
}

/// The addresses that are not private or local, or a refusal when none is.
pub(crate) fn public_addrs(
    host: &str,
    addrs: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, Refusal> {
    let public: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| !ap_ecosystem::net::is_forbidden_ip(a.ip()))
        .collect();
    if public.is_empty() {
        return Err(refuse(
            403,
            format!("{host} resolves only to private or local addresses"),
        ));
    }
    Ok(public)
}

fn connect_public(host: &str, port: u16) -> Result<TcpStream, Refusal> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| refuse(502, format!("cannot resolve {host}: {e}")))?;
    let mut last = None;
    for addr in public_addrs(host, addrs)? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(refuse(
        502,
        format!(
            "cannot connect to {host}:{port}: {}",
            last.map_or_else(|| "no address".to_string(), |e| e.to_string())
        ),
    ))
}

fn status_text(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        431 => "Request Header Fields Too Large",
        _ => "Bad Gateway",
    }
}

/// What the proxy threads share.
struct Shared {
    extra: Vec<String>,
    refused: Mutex<Vec<String>>,
    /// Upstream connections, shut down when the proxy stops so no tunnel
    /// thread outlives the run.
    active: Mutex<Vec<TcpStream>>,
}

/// A CONNECT-only proxy on a unix socket in a private directory, running
/// until dropped.
pub(crate) struct Proxy {
    dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl Proxy {
    /// Start serving; `extra` are the hosts allowed besides the fixed lists.
    pub(crate) fn start(extra: Vec<String>) -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("ap-service-proxy-")
            .tempdir()?;
        let listener = UnixListener::bind(dir.path().join(SOCKET_NAME))?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            extra,
            refused: Mutex::new(Vec::new()),
            active: Mutex::new(Vec::new()),
        });
        let accept = {
            let stop = Arc::clone(&stop);
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match conn {
                        Ok(client) => {
                            let shared = Arc::clone(&shared);
                            std::thread::spawn(move || handle(client, &shared));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })
        };
        Ok(Proxy {
            dir,
            stop,
            accept: Some(accept),
            shared,
        })
    }

    /// The directory holding the socket, bound into the sandbox.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The requests refused so far, one line each.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.shared
            .refused
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = UnixStream::connect(self.dir.path().join(SOCKET_NAME));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        if let Ok(active) = self.shared.active.lock() {
            for s in active.iter() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn handle(mut client: UnixStream, shared: &Shared) {
    let _ = client.set_read_timeout(Some(HEAD_TIMEOUT));
    let request = read_head(&mut client).and_then(|(head, rest)| {
        let (host, port) = parse_connect(&head)?;
        let host = check_target(&host, port, &shared.extra)
            .map_err(|e| refuse(403, format!("{host}:{port}: {e}")))?;
        Ok((host, port, rest))
    });
    let upstream = request.and_then(|(host, port, rest)| {
        connect_public(&host, port)
            .map_err(|r| refuse(r.status, format!("{host}:{port}: {}", r.reason)))
            .map(|s| (s, rest))
    });
    let (upstream, rest) = match upstream {
        Ok(x) => x,
        Err(r) => {
            if let Ok(mut refused) = shared.refused.lock() {
                refused.push(format!("{} {}", r.status, r.reason));
            }
            let body = format!("{}\n", r.reason);
            let _ = write!(
                client,
                "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                r.status,
                status_text(r.status),
                body.len()
            );
            return;
        }
    };
    if let (Ok(c), Ok(mut active)) = (upstream.try_clone(), shared.active.lock()) {
        active.push(c);
    }
    if client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let _ = client.set_read_timeout(None);
    tunnel(client, upstream, &rest);
}

/// Copy both ways until each side has closed.
fn tunnel(client: UnixStream, upstream: TcpStream, early: &[u8]) {
    let (Ok(mut client_r), Ok(mut upstream_w)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    if upstream_w.write_all(early).is_err() {
        return;
    }
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut client_r, &mut upstream_w);
        let _ = upstream_w.shutdown(Shutdown::Write);
    });
    let (mut upstream_r, mut client_w) = (upstream, client);
    let _ = io::copy(&mut upstream_r, &mut client_w);
    let _ = client_w.shutdown(Shutdown::Write);
    let _ = up.join();
}

// ── the sandbox ─────────────────────────────────────────────────────────────

/// Inside the sandbox: listen on the loopback port, hand each connection to
/// the proxy's unix socket, and run the command. The network namespace has no
/// other interface, so this is the only way out.
const BRIDGE: &str = r#"import socket, subprocess, sys, threading
path, port, cmd = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
lst = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
lst.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
lst.bind(("127.0.0.1", port))
lst.listen(128)
def pump(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    try:
        dst.shutdown(socket.SHUT_WR)
    except OSError:
        pass
def serve(conn):
    up = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        up.connect(path)
    except OSError:
        conn.close()
        return
    t = threading.Thread(target=pump, args=(up, conn), daemon=True)
    t.start()
    pump(conn, up)
    t.join()
    conn.close()
    up.close()
def accept():
    while True:
        conn, _ = lst.accept()
        threading.Thread(target=serve, args=(conn,), daemon=True).start()
threading.Thread(target=accept, daemon=True).start()
sys.exit(subprocess.call(cmd))
"#;

/// What one sandboxed run binds and sets.
pub(crate) struct Profile<'a> {
    /// The checkout, absolute and canonical: bound read-write at its path.
    pub checkout: &'a Path,
    /// Caches and temporary files, inside the checkout.
    pub scratch: &'a Path,
    /// The proxy's socket directory on the host.
    pub proxy_dir: &'a Path,
    /// More variables (the changelog identity).
    pub extra_env: &'a [(String, String)],
}

fn utf8(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

/// The environment inside the sandbox: nothing is inherited.
pub(crate) fn sandbox_env(p: &Profile<'_>) -> Result<Vec<(String, String)>, String> {
    let scratch = utf8(p.scratch)?;
    let proxy = format!("http://127.0.0.1:{PROXY_PORT}");
    let mut env: Vec<(String, String)> = [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        ("LANG", "C.UTF-8".to_string()),
        ("HOME", SANDBOX_HOME.to_string()),
        ("TMPDIR", format!("{scratch}/tmp")),
        ("XDG_CACHE_HOME", format!("{scratch}/cache")),
        ("CARGO_HOME", format!("{scratch}/cargo")),
        ("npm_config_cache", format!("{scratch}/npm")),
        ("GOPATH", format!("{scratch}/go")),
        ("GOMODCACHE", format!("{scratch}/go/pkg/mod")),
        // The module cache is read-only otherwise, and could not be removed.
        ("GOFLAGS", "-modcacherw".to_string()),
        ("OSC_VERSION", OSC_VERSION.to_string()),
        ("http_proxy", proxy.clone()),
        ("https_proxy", proxy.clone()),
        ("HTTP_PROXY", proxy.clone()),
        ("HTTPS_PROXY", proxy.clone()),
        ("no_proxy", String::new()),
        ("NO_PROXY", String::new()),
        ("CARGO_HTTP_PROXY", proxy.clone()),
        ("npm_config_proxy", proxy.clone()),
        ("npm_config_https_proxy", proxy.clone()),
        ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".to_string()),
        ("GIT_CONFIG_COUNT", "2".to_string()),
        ("GIT_CONFIG_KEY_0", "safe.directory".to_string()),
        ("GIT_CONFIG_VALUE_0", "*".to_string()),
        ("GIT_CONFIG_KEY_1", "http.proxy".to_string()),
        ("GIT_CONFIG_VALUE_1", proxy),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    env.extend(p.extra_env.iter().cloned());
    Ok(env)
}

/// The `bwrap` arguments that run `program` in the sandbox `p` describes.
pub(crate) fn bwrap_args(p: &Profile<'_>, program: &[String]) -> Result<Vec<String>, String> {
    let checkout = utf8(p.checkout)?;
    if !p.checkout.is_absolute() {
        return Err(format!("{checkout} is not absolute"));
    }
    let osc = format!("{checkout}/.osc");
    let proxy_dir = utf8(p.proxy_dir)?;
    let mut args: Vec<String> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]
    .map(String::from)
    .into();
    args.extend(ap_core::sandbox::system_ro_binds());
    args.extend(ap_core::sandbox::etc_ro_binds());
    args.extend(
        [
            // tar_scm's site configuration.
            "--ro-bind-try",
            "/etc/obs",
            "/etc/obs",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--dir",
            SANDBOX_HOME,
            "--bind",
            checkout,
            checkout,
            // osc commits to the API named in `.osc/_apiurl`, with the user's
            // credentials: a service must not be able to change it.
            "--ro-bind-try",
            &osc,
            &osc,
            "--ro-bind",
            proxy_dir,
            SANDBOX_PROXY_DIR,
            "--chdir",
            checkout,
            "--clearenv",
        ]
        .map(String::from),
    );
    for (k, v) in sandbox_env(p)? {
        args.extend(["--setenv".to_string(), k, v]);
    }
    args.extend(
        [
            "--",
            PYTHON,
            "-I",
            "-S",
            "-c",
            BRIDGE,
            &format!("{SANDBOX_PROXY_DIR}/{SOCKET_NAME}"),
            &PROXY_PORT.to_string(),
        ]
        .map(String::from),
    );
    args.extend(program.iter().cloned());
    Ok(args)
}

/// One `<service>` of a validated `_service`, its params decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceCall {
    pub name: String,
    pub params: Vec<(String, String)>,
}

/// The helper command line osc builds for `s`.
pub(crate) fn service_argv(s: &ServiceCall, outdir: &Path) -> Vec<String> {
    let mut argv = vec![format!("{SERVICE_DIR}/{}", s.name)];
    for (name, value) in &s.params {
        argv.push(format!("--{name}"));
        argv.push(value.clone());
    }
    argv.push("--outdir".to_string());
    argv.push(outdir.to_string_lossy().into_owned());
    argv
}

/// Whether services run unsandboxed on request; warns once when they do.
pub(crate) fn unsafe_mode() -> bool {
    let on = ap_core::env::env_flag(UNSAFE_ENV);
    if on {
        static WARNED: Once = Once::new();
        WARNED.call_once(|| {
            ap_core::diag!(
                "WARNING: {UNSAFE_ENV} is set: `osc service` runs WITHOUT a sandbox, with your \
                 home directory, the session bus (your OBS credentials) and the whole network \
                 reachable by the sources it fetches"
            );
        });
    }
    on
}

/// The `bwrap` to run, or why services cannot be sandboxed here.
fn sandbox_ready() -> Result<&'static Path, Error> {
    let why = match ap_core::sandbox::bwrap_path() {
        None => "bwrap is not installed".to_string(),
        Some(_) if !ap_core::sandbox::bwrap_works() => {
            "bwrap cannot create user namespaces here".to_string()
        }
        Some(_) if !Path::new(PYTHON).is_file() => format!("{PYTHON} is missing"),
        Some(bwrap) => return Ok(bwrap),
    };
    Err(Error::ServiceSandbox(format!(
        "{why}. Install bubblewrap and allow unprivileged user namespaces \
         (sysctl user.max_user_namespaces=15000; kernel.unprivileged_userns_clone=1 where it \
         exists; kernel.apparmor_restrict_unprivileged_userns=0 on AppArmor systems), or set \
         {UNSAFE_ENV}=1 to run services unsandboxed"
    )))
}

/// The changelog identity tar_scm's `changesgenerate` needs, as osc would
/// export it: `VC_REALNAME`/`VC_MAILADDR` from the environment, else
/// `realname`/`email` of the checkout's API in the oscrc. Nothing else of the
/// oscrc is read.
fn changes_identity(checkout: &Path) -> Vec<(String, String)> {
    let from_env: Vec<(String, String)> = ["VC_REALNAME", "VC_MAILADDR"]
        .iter()
        .filter_map(|k| {
            let v = std::env::var(k).ok().filter(|v| !v.trim().is_empty())?;
            Some((k.to_string(), v))
        })
        .collect();
    if !from_env.is_empty() {
        return from_env;
    }
    let Ok(apiurl) = fs::read_to_string(checkout.join(".osc").join("_apiurl")) else {
        return Vec::new();
    };
    oscrc_paths()
        .into_iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .map(|text| oscrc_identity(&text, apiurl.trim()))
        .unwrap_or_default()
}

fn oscrc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = std::env::var_os("OSC_CONFIG") {
        paths.push(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    if let Some(c) = config {
        paths.push(c.join("osc").join("oscrc"));
    }
    if let Some(h) = home {
        paths.push(h.join(".oscrc"));
    }
    paths
}

/// `realname` and `email` from the oscrc section of `apiurl`.
fn oscrc_identity(text: &str, apiurl: &str) -> Vec<(String, String)> {
    fn norm(url: &str) -> String {
        let url = url.trim().to_ascii_lowercase();
        let url = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(&url);
        url.trim_end_matches('/').to_string()
    }
    let want = norm(apiurl);
    let mut in_section = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = norm(name) == want;
            continue;
        }
        if !in_section || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some((key, value)) = line.split_once(['=', ':']) {
            let var = match key.trim() {
                "realname" => "VC_REALNAME",
                "email" => "VC_MAILADDR",
                _ => continue,
            };
            let value = value.trim();
            if !value.is_empty() {
                out.push((var.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// Remove what an earlier run generated, as osc does before running.
fn remove_generated(checkout: &Path) {
    let Ok(rd) = fs::read_dir(checkout) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("_service:") || name.starts_with("_service_") {
            let path = entry.path();
            let _ = if path.is_dir() && !path.is_symlink() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

fn top_level_symlinks(dir: &Path) -> Vec<std::ffi::OsString> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                .map(|e| e.file_name())
                .collect()
        })
        .unwrap_or_default()
}

/// Move a service's outputs into the checkout, as osc does. A symbolic link
/// (which could point the pipeline at a host file) and `.osc` are refused.
fn move_outputs(service: &str, outdir: &Path, checkout: &Path) -> Result<(), Error> {
    let rejected = |reason: String| Error::ServiceRejected { reason };
    for entry in fs::read_dir(outdir).map_err(Error::Io)?.flatten() {
        let name = entry.file_name();
        if entry.file_type().map_err(Error::Io)?.is_symlink() {
            return Err(rejected(format!(
                "service {service} produced a symbolic link `{}`",
                name.to_string_lossy()
            )));
        }
        if name == ".osc" {
            return Err(rejected(format!("service {service} produced `.osc`")));
        }
        let target = checkout.join(&name);
        if target.is_dir() && !target.is_symlink() {
            fs::remove_dir_all(&target).map_err(Error::Io)?;
        }
        fs::rename(entry.path(), &target).map_err(Error::Io)?;
    }
    Ok(())
}

/// Run `services` (from the checkout's validated `_service`) in the sandbox,
/// in order, each the way `osc service ra` would. The result is the failing
/// service's output, with the proxy's refusals appended, or a success.
pub(crate) fn run_services(
    checkout_dir: &Path,
    services: &[ServiceCall],
    emit: Emit<'_>,
) -> Result<Captured, Error> {
    let ok = Captured {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    if services.is_empty() {
        return Ok(ok);
    }
    let bwrap = sandbox_ready()?;
    for s in services {
        if !Path::new(SERVICE_DIR).join(&s.name).is_file() {
            return Err(Error::ServiceSandbox(format!(
                "obs-service-{} is not installed ({SERVICE_DIR}/{} is missing)",
                s.name, s.name
            )));
        }
    }
    let checkout = fs::canonicalize(checkout_dir).map_err(Error::Io)?;
    remove_generated(&checkout);
    let links_before = top_level_symlinks(&checkout);
    let scratch = tempfile::Builder::new()
        .prefix(".ap-service-")
        .tempdir_in(&checkout)
        .map_err(Error::Io)?;
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.path().join(sub)).map_err(Error::Io)?;
    }
    let proxy = Proxy::start(extra_source_hosts()).map_err(Error::Io)?;
    let identity = changes_identity(&checkout);
    let profile = Profile {
        checkout: &checkout,
        scratch: scratch.path(),
        proxy_dir: proxy.dir(),
        extra_env: &identity,
    };
    let cmd = format!("service ra in {}", checkout_dir.display());
    let bwrap = bwrap.to_string_lossy();
    for s in services {
        let outdir = tempfile::Builder::new()
            .prefix(".")
            .suffix(&format!(".{}.service", s.name))
            .tempdir_in(&checkout)
            .map_err(Error::Io)?;
        let args =
            bwrap_args(&profile, &service_argv(s, outdir.path())).map_err(Error::ServiceSandbox)?;
        let out = proc::capture_with_timeout_env(
            &bwrap,
            &args,
            &checkout,
            emit,
            proc::DEFAULT_CAPTURE_TIMEOUT,
            Some(&BTreeMap::new()),
        )
        .map_err(|source| Error::Command {
            cmd: cmd.clone(),
            source,
        })?;
        if !out.success() {
            let mut stderr = format!("service {} failed:\n{}", s.name, out.stderr.trim_end());
            for r in proxy.refusals() {
                stderr.push_str(&format!("\nsandbox proxy refused: {r}"));
            }
            return Ok(Captured {
                code: out.code,
                stdout: out.stdout,
                stderr,
            });
        }
        move_outputs(&s.name, outdir.path(), &checkout)?;
    }
    for name in top_level_symlinks(&checkout) {
        if !links_before.contains(&name) {
            let _ = fs::remove_file(checkout.join(&name));
            return Err(Error::ServiceRejected {
                reason: format!(
                    "a service created the symbolic link `{}` in the checkout",
                    name.to_string_lossy()
                ),
            });
        }
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn listed_hosts_and_their_subdomains_are_allowed() {
        for h in [
            "github.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "index.crates.io",
            "static.crates.io",
            "proxy.golang.org",
            "registry.npmjs.org",
            "files.pythonhosted.org",
            "gitlab.gnome.org",
            "netix.dl.sourceforge.net",
            // trusted source hosts and fixed forges come from the shared lists
            "ftp.gnu.org",
            "codeberg.org",
        ] {
            assert!(check_target(h, 443, &none()).is_ok(), "{h}");
        }
    }

    #[test]
    fn other_hosts_ports_and_forms_are_refused() {
        for (h, port) in [
            ("evil.com", 443),
            ("github.com.evil.com", 443),
            ("evilgithub.com", 443),
            ("raw.githubusercontent.com", 443),
            ("github.com", 80),
            ("github.com", 22),
            ("github.com", 8443),
            ("127.0.0.1", 443),
            ("140.82.112.3", 443),
            ("[::1]", 443),
            ("::1", 443),
            ("2130706433", 443),
            ("0x7f.1", 443),
            ("", 443),
            ("github..com", 443),
            ("-github.com", 443),
            ("user@github.com", 443),
            ("github.com/", 443),
            // an unencoded IDN lookalike, and punycode ones off the list
            ("gіthub.com", 443),
            ("xn--gthub-n4a.com", 443),
            ("github.xn--com-9o0a", 443),
        ] {
            assert!(check_target(h, port, &none()).is_err(), "{h}:{port}");
        }
    }

    #[test]
    fn case_and_one_trailing_dot_are_normalized() {
        assert_eq!(
            check_target("GitHub.COM.", 443, &none()).unwrap(),
            "github.com"
        );
        assert!(check_target("github.com..", 443, &none()).is_err());
    }

    #[test]
    fn extra_hosts_extend_the_list() {
        let extra = vec!["mirror.example.org".to_string()];
        assert!(check_target("mirror.example.org", 443, &extra).is_ok());
        assert!(check_target("a.mirror.example.org", 443, &extra).is_ok());
        assert!(check_target("example.org", 443, &extra).is_err());
        assert!(check_target("mirror.example.org", 80, &extra).is_err());
    }

    #[test]
    fn service_urls_must_be_https_on_an_allowed_host() {
        for ok in [
            "https://github.com/o/r.git",
            "https://GITHUB.com/o/r",
            "https://github.com:443/o/r",
            "https://crates.io",
            "https://gitlab.gnome.org/GNOME/x.git?x=1",
        ] {
            assert!(check_service_url_in(ok, &none()).is_ok(), "{ok}");
        }
        for (bad, says) in [
            ("file:///etc/passwd", "file://"),
            ("ssh://git@github.com/o/r", "ssh://"),
            ("git://github.com/o/r", "git://"),
            ("http://github.com/o/r", "http://"),
            ("git@github.com:o/r.git", "https://"),
            ("https://user@github.com/o/r", "user info"),
            ("https://u:p@github.com/o/r", "user info"),
            ("https://github.com@evil.com/o/r", "user info"),
            ("https://evil.com/o/r", "not an allowed"),
            ("https://github.com.evil.com/o", "not an allowed"),
            ("https://github.com:22/o/r", "port"),
            ("https://127.0.0.1/x", "IP"),
            ("https://[::1]/x", "IP"),
            ("https://github.com/o r", "whitespace"),
            ("https://", "not a valid"),
        ] {
            let err = check_service_url_in(bad, &none()).unwrap_err();
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn connect_requests_are_parsed() {
        assert_eq!(
            parse_connect("CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\r\n"),
            Ok(("github.com".to_string(), 443))
        );
        let status = |head: &str| parse_connect(head).unwrap_err().status;
        assert_eq!(status("GET http://github.com/ HTTP/1.1\r\n\r\n"), 405);
        assert_eq!(status("CONNECT github.com HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("CONNECT github.com:x HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("CONNECT github.com:99999 HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("CONNECT [::1]:443 HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("CONNECT u@github.com:443 HTTP/1.1\r\n\r\n"), 403);
        assert_eq!(status("CONNECT github.com:443 SPDY/3\r\n\r\n"), 400);
        assert_eq!(status("CONNECT  github.com:443 HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("\r\n\r\n"), 400);
        let many = format!(
            "CONNECT github.com:443 HTTP/1.1\r\n{}\r\n",
            "X: y\r\n".repeat(MAX_HEADER_LINES + 1)
        );
        assert_eq!(status(&many), 431);
    }

    #[test]
    fn request_heads_are_bounded() {
        let mut r: &[u8] = b"CONNECT a:443 HTTP/1.1\r\n\r\n\x16\x03";
        let (head, rest) = read_head(&mut r).unwrap();
        assert_eq!(head, "CONNECT a:443 HTTP/1.1\r\n\r\n");
        assert_eq!(rest, b"\x16\x03");
        let long = format!(
            "CONNECT a:443 HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEAD)
        );
        assert_eq!(read_head(&mut long.as_bytes()).unwrap_err().status, 431);
        let endless = "a".repeat(MAX_HEAD * 2);
        assert_eq!(read_head(&mut endless.as_bytes()).unwrap_err().status, 431);
        assert_eq!(
            read_head(&mut &b"CONNECT a:443"[..]).unwrap_err().status,
            400
        );
        assert_eq!(
            read_head(&mut &b"\xff\xfe\r\n\r\n"[..]).unwrap_err().status,
            400
        );
    }

    #[test]
    fn private_addresses_are_dropped() {
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        let got = public_addrs(
            "h",
            [
                addr("127.0.0.1:443"),
                addr("10.1.2.3:443"),
                addr("169.254.169.254:443"),
                addr("[::1]:443"),
                addr("[fe80::1]:443"),
                addr("140.82.112.3:443"),
            ],
        )
        .unwrap();
        assert_eq!(got, vec![addr("140.82.112.3:443")]);
        let err = public_addrs("h", [addr("192.168.1.1:443"), addr("[fc00::1]:443")]).unwrap_err();
        assert_eq!(err.status, 403);
    }

    /// Send `request` to a running proxy and return its status line.
    fn ask(proxy: &Proxy, request: &[u8]) -> String {
        let mut s = UnixStream::connect(proxy.dir().join(SOCKET_NAME)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(request).unwrap();
        let mut answer = String::new();
        let _ = s.read_to_string(&mut answer);
        answer.lines().next().unwrap_or("").to_string()
    }

    #[test]
    fn the_proxy_refuses_without_touching_the_network() {
        // `localhost` is allowed here only to show that an allowed name that
        // resolves to a loopback address (from /etc/hosts) is still refused.
        let proxy = Proxy::start(vec!["localhost".to_string()]).unwrap();
        for (req, status) in [
            (&b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n"[..], "403"),
            (b"CONNECT github.com:80 HTTP/1.1\r\n\r\n", "403"),
            (b"CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n", "403"),
            (b"CONNECT localhost:443 HTTP/1.1\r\n\r\n", "403"),
            (b"GET http://github.com/ HTTP/1.1\r\n\r\n", "405"),
        ] {
            let line = ask(&proxy, req);
            assert!(line.starts_with(&format!("HTTP/1.1 {status}")), "{line}");
        }
        let refused = proxy.refusals().join("\n");
        assert!(refused.contains("evil.example"), "{refused}");
        assert!(refused.contains("private or local"), "{refused}");
    }

    fn profile_args(env: &[(String, String)]) -> Vec<String> {
        let p = Profile {
            checkout: Path::new("/work/proj/pkg"),
            scratch: Path::new("/work/proj/pkg/.ap-service-x"),
            proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
            extra_env: env,
        };
        bwrap_args(
            &p,
            &["/usr/lib/obs/service/tar_scm".to_string(), "--url".into()],
        )
        .unwrap()
    }

    /// The value after each occurrence of `flag`, `n` values long.
    fn after(args: &[String], flag: &str, n: usize) -> Vec<Vec<String>> {
        args.iter()
            .enumerate()
            .filter(|(_, a)| *a == flag)
            .map(|(i, _)| args[i + 1..i + 1 + n].to_vec())
            .collect()
    }

    #[test]
    fn the_sandbox_binds_only_the_checkout_and_clears_the_environment() {
        let args = profile_args(&[]);
        let pos = |f: &str| args.iter().position(|a| a == f);
        for f in [
            "--unshare-all",
            "--die-with-parent",
            "--clearenv",
            "--new-session",
        ] {
            assert!(pos(f).is_some(), "{f}: {args:?}");
        }
        assert!(pos("--share-net").is_none(), "{args:?}");
        // The only read-write bind is the checkout, at its own path.
        assert_eq!(
            after(&args, "--bind", 2),
            vec![vec!["/work/proj/pkg", "/work/proj/pkg"]]
        );
        assert!(after(&args, "--bind-try", 2).is_empty());
        // Its .osc is read-only, bound after it.
        let osc = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind-try" && w[1] == "/work/proj/pkg/.osc")
            .unwrap();
        assert!(osc > pos("--bind").unwrap());
        assert!(
            after(&args, "--ro-bind", 2).contains(&vec![
                "/tmp/ap-service-proxy-x".to_string(),
                SANDBOX_PROXY_DIR.to_string()
            ]),
            "{args:?}"
        );
        // No home directory, session bus or osc configuration is mounted.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        for a in &args {
            assert!(!a.contains("/run/user") && !a.contains("bus"), "{a}");
            assert!(!a.contains("oscrc") && !a.contains(".config"), "{a}");
            assert!(!a.starts_with(&home) || home == "/", "{a}");
        }
        // The environment is set after --clearenv and holds no session bus.
        let env = after(&args, "--setenv", 2);
        assert!(args.iter().position(|a| a == "--setenv").unwrap() > pos("--clearenv").unwrap());
        let get = |k: &str| env.iter().find(|kv| kv[0] == k).map(|kv| kv[1].clone());
        assert_eq!(get("HOME").as_deref(), Some(SANDBOX_HOME));
        assert!(get("DBUS_SESSION_BUS_ADDRESS").is_none());
        let proxy = format!("http://127.0.0.1:{PROXY_PORT}");
        for k in [
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "CARGO_HTTP_PROXY",
            "npm_config_https_proxy",
        ] {
            assert_eq!(get(k).as_deref(), Some(proxy.as_str()), "{k}");
        }
        assert_eq!(get("GIT_CONFIG_VALUE_1").as_deref(), Some(proxy.as_str()));
        assert_eq!(
            get("CARGO_HOME").as_deref(),
            Some("/work/proj/pkg/.ap-service-x/cargo")
        );
        assert_eq!(
            get("npm_config_cache").as_deref(),
            Some("/work/proj/pkg/.ap-service-x/npm")
        );
        // The program runs behind the bridge, at the end.
        let dash = pos("--").unwrap();
        assert_eq!(args[dash + 1], PYTHON);
        assert_eq!(
            args[args.len() - 2..],
            ["/usr/lib/obs/service/tar_scm", "--url"]
        );
    }

    #[test]
    fn extra_env_is_appended_and_relative_checkouts_are_refused() {
        let args = profile_args(&[("VC_MAILADDR".into(), "a@b".into())]);
        assert!(after(&args, "--setenv", 2).contains(&vec!["VC_MAILADDR".into(), "a@b".into()]));
        let p = Profile {
            checkout: Path::new("pkg"),
            scratch: Path::new("pkg/s"),
            proxy_dir: Path::new("/tmp/p"),
            extra_env: &[],
        };
        assert!(bwrap_args(&p, &[]).is_err());
    }

    #[test]
    fn the_service_command_line_is_the_one_osc_builds() {
        let s = ServiceCall {
            name: "tar_scm".into(),
            params: vec![
                ("url".into(), "https://github.com/o/r.git".into()),
                ("legacy".into(), String::new()),
            ],
        };
        assert_eq!(
            service_argv(&s, Path::new("/c/.x.tar_scm.service")),
            [
                "/usr/lib/obs/service/tar_scm",
                "--url",
                "https://github.com/o/r.git",
                "--legacy",
                "",
                "--outdir",
                "/c/.x.tar_scm.service"
            ]
        );
    }

    #[test]
    fn the_identity_comes_from_the_apis_section_only() {
        let rc = "[general]\nemail = no@x\n\n[https://api.opensuse.org/]\nuser = u\npass = secret\nemail = me@example.com\nrealname = Me Myself\n[https://other]\nemail = other@x\n";
        let got = oscrc_identity(rc, "https://api.opensuse.org");
        assert_eq!(
            got,
            vec![
                ("VC_MAILADDR".to_string(), "me@example.com".to_string()),
                ("VC_REALNAME".to_string(), "Me Myself".to_string())
            ]
        );
        assert!(oscrc_identity(rc, "https://api.suse.de").is_empty());
    }

    #[test]
    fn outputs_are_moved_and_links_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(".o.tar_scm.service");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("a.tar.gz"), "x").unwrap();
        move_outputs("tar_scm", &out, dir.path()).unwrap();
        assert!(dir.path().join("a.tar.gz").is_file());
        std::os::unix::fs::symlink("/etc/passwd", out.join("b.spec")).unwrap();
        assert!(move_outputs("tar_scm", &out, dir.path()).is_err());
        assert!(!dir.path().join("b.spec").exists());
    }

    /// Why the bwrap test cannot run here, if it cannot.
    fn no_sandbox() -> Option<&'static str> {
        if !ap_core::sandbox::bwrap_works() {
            return Some("bwrap is missing or user namespaces are disabled");
        }
        if !Path::new(PYTHON).is_file() {
            return Some("python3 is missing");
        }
        None
    }

    #[test]
    fn the_sandbox_isolates_home_bus_network_and_osc_metadata() {
        if let Some(why) = no_sandbox() {
            eprintln!("skipped: {why}");
            return;
        }
        let checkout = tempfile::tempdir().unwrap();
        let checkout_path = fs::canonicalize(checkout.path()).unwrap();
        fs::create_dir(checkout_path.join(".osc")).unwrap();
        fs::write(
            checkout_path.join(".osc/_apiurl"),
            "https://api.opensuse.org\n",
        )
        .unwrap();
        let scratch = checkout_path.join(".ap-service-t");
        fs::create_dir_all(scratch.join("tmp")).unwrap();
        let proxy = Proxy::start(Vec::new()).unwrap();
        let p = Profile {
            checkout: &checkout_path,
            scratch: &scratch,
            proxy_dir: proxy.dir(),
            extra_env: &[],
        };
        let real_home = std::env::var("HOME").unwrap_or_default();
        let probe = format!(
            r#"import os, socket, subprocess
print("uid", os.getuid())
print("home", sorted(os.listdir(os.environ["HOME"])))
print("realhome", os.path.exists({real_home:?}) and {real_home:?} != "/" and sorted(os.listdir({real_home:?})))
print("bus", "DBUS_SESSION_BUS_ADDRESS" in os.environ, os.path.exists("/run/user/%d/bus" % os.getuid()))
print("true", subprocess.call(["true"]))
try:
    socket.create_connection(("1.1.1.1", 443), timeout=3)
    print("direct", "open")
except OSError as e:
    print("direct", e.errno)
s = socket.create_connection(("127.0.0.1", {PROXY_PORT}), timeout=10)
s.sendall(b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n")
buf = b""
while b"\r\n" not in buf:
    chunk = s.recv(64)
    if not chunk:
        break
    buf += chunk
print("proxy", buf.split(b"\r\n")[0].decode())
open("written", "w").write("x")
try:
    open(".osc/_apiurl", "w").write("https://evil.example")
    print("osc", "writable")
except OSError as e:
    print("osc", e.errno)
"#
        );
        let args = bwrap_args(&p, &["python3".into(), "-I".into(), "-c".into(), probe]).unwrap();
        let out = std::process::Command::new(ap_core::sandbox::bwrap_path().unwrap())
            .env_clear()
            .args(&args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let uid = unsafe { libc::getuid() };
        assert!(stdout.contains(&format!("uid {uid}\n")), "{stdout}");
        assert!(stdout.contains("home []\n"), "{stdout}");
        assert!(stdout.contains("realhome False\n"), "{stdout}");
        assert!(stdout.contains("bus False False\n"), "{stdout}");
        assert!(stdout.contains("true 0\n"), "{stdout}");
        // ENETUNREACH: the namespace has only the loopback interface.
        assert!(stdout.contains("direct 101\n"), "{stdout}");
        assert!(
            stdout.contains("proxy HTTP/1.1 403 Forbidden\n"),
            "{stdout}"
        );
        // EROFS
        assert!(stdout.contains("osc 30\n"), "{stdout}");
        assert!(checkout_path.join("written").is_file());
        assert_eq!(
            fs::read_to_string(checkout_path.join(".osc/_apiurl")).unwrap(),
            "https://api.opensuse.org\n"
        );
    }
}
