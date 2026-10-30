use super::*;

pub(super) fn test_asker() -> crate::allowed_hosts::Asker {
    crate::allowed_hosts::Asker::service("test-pkg", "test")
}

fn none() -> AllowedHosts {
    AllowedHosts::empty()
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
        assert!(check_target(h, 443, &none(), &test_asker()).is_ok(), "{h}");
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
        assert!(
            check_target(h, port, &none(), &test_asker()).is_err(),
            "{h}:{port}"
        );
    }
}

#[test]
fn case_and_one_trailing_dot_are_normalized() {
    assert_eq!(
        check_target("GitHub.COM.", 443, &none(), &test_asker()).unwrap(),
        "github.com"
    );
    assert!(check_target("github.com..", 443, &none(), &test_asker()).is_err());
}

#[test]
fn extra_hosts_extend_the_list() {
    let extra = AllowedHosts::with_env("mirror.example.org");
    assert!(check_target("mirror.example.org", 443, &extra, &test_asker()).is_ok());
    assert!(check_target("a.mirror.example.org", 443, &extra, &test_asker()).is_ok());
    assert!(check_target("example.org", 443, &extra, &test_asker()).is_err());
    assert!(check_target("mirror.example.org", 80, &extra, &test_asker()).is_err());
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
        assert!(
            check_service_url_in(ok, &none(), &test_asker()).is_ok(),
            "{ok}"
        );
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
        ("https://evil.com/o/r", "is not allowed"),
        ("https://github.com.evil.com/o", "is not allowed"),
        ("https://github.com:22/o/r", "port"),
        ("https://127.0.0.1/x", "IP"),
        ("https://[::1]/x", "IP"),
        ("https://github.com/o r", "whitespace"),
        ("https://", "not a valid"),
    ] {
        let err = check_service_url_in(bad, &none(), &test_asker()).unwrap_err();
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
    let hosts = AllowedHosts::empty();
    hosts
        .allow("localhost", crate::allowed_hosts::Origin::Env, None)
        .unwrap();
    let proxy = Proxy::start(Arc::new(hosts), test_asker()).unwrap();
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
        hosts_file: Path::new("/tmp/ap-service-proxy-x/hosts"),
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
        hosts_file: Path::new("/tmp/p/hosts"),
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
pub(super) fn no_sandbox() -> Option<&'static str> {
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
    let proxy = Proxy::start(Arc::new(AllowedHosts::empty()), test_asker()).unwrap();
    let hosts = proxy.write_hosts("hosts", &[]).unwrap();
    let p = Profile {
        checkout: &checkout_path,
        scratch: &scratch,
        proxy_dir: proxy.dir(),
        extra_env: &[],
        hosts_file: &hosts,
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

/// Probe: what a service reaches through the sandbox proxy, with the network.
/// Ignored by default; run with
/// `cargo test -p ap-build probe_services_fetch_through_the_proxy -- --ignored --nocapture`.
///
/// Findings (cargo_vendor helper 7.2.2 with libgit2 1.9.7, git 2.56, cargo 1.96):
/// the git CLI and cargo reach github.com and the crates.io hosts through the
/// proxy, from the environment the sandbox sets. cargo_vendor's own clone
/// (libroast, through libgit2) does not use the proxy: it resolves the host
/// itself and fails with `failed to resolve address for github.com`.
/// `https_proxy`, a `GIT_CONFIG_GLOBAL` file with `[http] proxy` and a
/// `$HOME/.gitconfig` with `[http] proxy` made no difference.
#[test]
#[ignore]
fn probe_services_fetch_through_the_proxy() {
    if let Some(why) = no_sandbox() {
        panic!("cannot probe here: {why}");
    }
    let root = tempfile::tempdir().unwrap();
    let checkout = fs::canonicalize(root.path()).unwrap();
    let scratch = checkout.join(".ap-service-probe");
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.join(sub)).unwrap();
    }
    fs::create_dir_all(checkout.join("c/src")).unwrap();
    fs::write(
        checkout.join("c/Cargo.toml"),
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nitoa = \"1\"\n",
    )
    .unwrap();
    fs::write(checkout.join("c/src/lib.rs"), "").unwrap();
    let proxy = Proxy::start(crate::allowed_hosts::shared(), test_asker()).unwrap();
    let hosts = proxy.write_hosts("hosts", &[]).unwrap();
    let run = |program: &[&str]| {
        let p = Profile {
            checkout: &checkout,
            scratch: &scratch,
            proxy_dir: proxy.dir(),
            extra_env: &[],
            hosts_file: &hosts,
        };
        let program: Vec<String> = program.iter().map(|s| s.to_string()).collect();
        let args = bwrap_args(&p, &program).unwrap();
        std::process::Command::new(ap_core::sandbox::bwrap_path().unwrap())
            .env_clear()
            .args(&args)
            .output()
            .unwrap()
    };
    let text = |out: &std::process::Output| {
        format!(
            "{}{}\nrefused: {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            proxy.refusals()
        )
    };

    let git = run(&["git", "ls-remote", "https://github.com/mahkoh/jay", "HEAD"]);
    eprintln!("probe git ls-remote: {}", text(&git));
    assert!(git.status.success(), "{}", text(&git));

    let cargo = run(&["sh", "-c", "cd c && cargo fetch"]);
    eprintln!("probe cargo fetch: {}", text(&cargo));
    assert!(cargo.status.success(), "{}", text(&cargo));
}
