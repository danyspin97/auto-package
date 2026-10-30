use super::*;

// ── the proxy ───────────────────────────────────────────────────────────────

/// Longest request head the proxy reads.
pub(super) const MAX_HEAD: usize = 8 * 1024;

/// Most header lines after the request line.
pub(super) const MAX_HEADER_LINES: usize = 64;

/// Time a client has to send its request.
pub(super) const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Why the proxy answers a request with an error.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub status: u16,
    pub reason: String,
}

pub(super) fn refuse(status: u16, reason: impl Into<String>) -> Refusal {
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

pub(super) fn connect_public(host: &str, port: u16) -> Result<TcpStream, Refusal> {
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

pub(super) fn status_text(status: u16) -> &'static str {
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
pub(super) struct Shared {
    /// Read for every request, so a host allowed during the run applies.
    hosts: Arc<AllowedHosts>,
    /// Who the requests are for: recorded with each refused host.
    asker: Asker,
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
    /// Start serving; `hosts` is the allowlist every request is checked against,
    /// and `asker` names what the requests are for.
    pub(crate) fn start(hosts: Arc<AllowedHosts>, asker: Asker) -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("ap-service-proxy-")
            .tempdir()?;
        let listener = UnixListener::bind(dir.path().join(SOCKET_NAME))?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            hosts,
            asker,
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

    /// Write a hosts file for the sandbox into the socket directory, with
    /// `map` (see [`bridge_addresses`]). It is bound over the sandbox's
    /// `/etc/hosts`.
    pub(crate) fn write_hosts(&self, name: &str, map: &[(String, String)]) -> io::Result<PathBuf> {
        let path = self.dir.path().join(name);
        fs::write(&path, hosts_file_text(map))?;
        Ok(path)
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

pub(super) fn handle(mut client: UnixStream, shared: &Shared) {
    let _ = client.set_read_timeout(Some(HEAD_TIMEOUT));
    let request = read_head(&mut client).and_then(|(head, rest)| {
        let (host, port) = parse_connect(&head)?;
        let host = check_target(&host, port, &shared.hosts, &shared.asker)
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
pub(super) fn tunnel(client: UnixStream, upstream: TcpStream, early: &[u8]) {
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
pub(super) const BRIDGE: &str = r#"import socket, subprocess, sys, threading
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
