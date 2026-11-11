use super::*;
use crate::allowed_hosts::{Origin, Use, Via, normalize_host};
use crate::osc::SOURCE_PARAMS;

fn params(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn asker() -> Asker {
    Asker::service("test-pkg", "test")
}

#[test]
fn the_source_hosts_are_the_url_params_normalized() {
    let p = params(&[
        ("src", "https://GitHub.com./mahkoh/jay"),
        ("revision", "v1.9.1"),
        ("src", "pkg"),
        ("url", "https://gitlab.com/x/y"),
    ]);
    let allowed = AllowedHosts::empty();
    assert_eq!(
        source_hosts(&p, &allowed, &asker()).unwrap(),
        ["github.com", "gitlab.com"]
    );
}

#[test]
fn every_source_param_has_its_url_host_extracted() {
    let allowed = AllowedHosts::empty();
    for &name in SOURCE_PARAMS {
        let p = params(&[(name, "https://github.com/x/y")]);
        assert_eq!(
            source_hosts(&p, &allowed, &asker()).unwrap(),
            ["github.com"],
            "{name}"
        );
    }
}

#[test]
fn a_source_url_with_an_address_or_a_forbidden_host_is_refused() {
    let allowed = AllowedHosts::empty();
    let ip = params(&[("src", "https://127.0.0.1/x")]);
    assert!(source_hosts(&ip, &allowed, &asker()).is_err());
    let other = params(&[("url", "https://example.org/x")]);
    assert!(source_hosts(&other, &allowed, &asker()).is_err());
}

#[test]
fn a_host_allowed_at_runtime_is_mapped_by_the_next_call() {
    let allowed = AllowedHosts::empty();
    let p = params(&[("src", "https://mirror.example.org/x/y")]);
    // Refused before the user allows it, and the refusal is recorded.
    assert!(source_hosts(&p, &allowed, &asker()).is_err());
    assert!(
        allowed
            .refused_hosts()
            .contains(&("mirror.example.org".to_string(), Use::Service))
    );
    assert!(
        allowed
            .refusals()
            .iter()
            .any(|(host, _, who)| host == "mirror.example.org" && *who == asker())
    );
    allowed
        .allow("mirror.example.org", Origin::Run(Via::Tui), None)
        .unwrap();
    let names = source_hosts(&p, &allowed, &asker()).unwrap();
    assert_eq!(names, ["mirror.example.org"]);
    assert_eq!(
        bridge_addresses(&names, &allowed, &asker()).unwrap(),
        [("127.0.0.2".to_string(), "mirror.example.org".to_string())]
    );
}

#[test]
fn a_service_url_host_the_user_has_not_allowed_is_refused_before_mapping() {
    let allowed = AllowedHosts::empty();
    let url = "https://mirror.example.org/x";
    let err = check_service_url_in(url, &allowed, &asker()).unwrap_err();
    assert!(err.contains("is not allowed"), "{err}");
    let err = source_hosts(&params(&[("url", url)]), &allowed, &asker()).unwrap_err();
    assert!(
        err.starts_with("url `https://mirror.example.org/x`"),
        "{err}"
    );
}

#[test]
fn each_host_gets_its_own_loopback_address_in_name_order() {
    let allowed = AllowedHosts::empty();
    let hosts = ["gitlab.com", "GitHub.com.", "github.com"].map(String::from);
    let map = bridge_addresses(&hosts, &allowed, &asker()).unwrap();
    assert_eq!(
        map,
        [
            ("127.0.0.2".to_string(), "github.com".to_string()),
            ("127.0.0.3".to_string(), "gitlab.com".to_string()),
        ]
    );
}

#[test]
fn the_hosts_file_starts_with_localhost_then_holds_only_allowed_names() {
    let allowed = AllowedHosts::empty();
    let hosts = ["gitlab.com", "github.com"].map(String::from);
    let map = bridge_addresses(&hosts, &allowed, &asker()).unwrap();
    let text = hosts_file_text(&map);
    assert_eq!(
        text,
        "127.0.0.1 localhost\n::1 localhost\n127.0.0.2 github.com\n127.0.0.3 gitlab.com\n"
    );
    assert_eq!(hosts_file_text(&[]), "127.0.0.1 localhost\n::1 localhost\n");
    for line in text.lines().skip(2) {
        let (ip, name) = line.split_once(' ').unwrap();
        assert!(ip.starts_with("127.0.0."), "{line}");
        assert_eq!(normalize_host(name).unwrap(), name, "{line}");
        assert!(allowed.allows(name, Use::Service), "{line}");
    }
}

#[test]
fn a_name_that_is_not_allowed_or_is_an_address_maps_nowhere() {
    let allowed = AllowedHosts::empty();
    assert!(bridge_addresses(&["example.org".to_string()], &allowed, &asker()).is_err());
    assert!(bridge_addresses(&["127.0.0.1".to_string()], &allowed, &asker()).is_err());
}

#[test]
fn the_sandbox_has_an_address_for_each_mapped_host_and_no_more() {
    let allowed = AllowedHosts::empty();
    let names = |n: usize| -> Vec<String> { (0..n).map(|i| format!("h{i}.github.com")).collect() };
    assert_eq!(
        bridge_addresses(&names(250), &allowed, &asker())
            .unwrap()
            .len(),
        250
    );
    assert!(bridge_addresses(&names(251), &allowed, &asker()).is_err());
}

/// Run `program` in the sandbox with the hosts file and the tunnels of `map`;
/// the outcome, and the proxy's refusals.
fn run_mapped(map: &[(String, String)], program: &[&str]) -> (std::process::Output, Vec<String>) {
    let root = tempfile::tempdir().unwrap();
    let checkout = fs::canonicalize(root.path()).unwrap();
    let scratch = checkout.join(".ap-service-t");
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.join(sub)).unwrap();
    }
    let proxy = Proxy::start(Arc::new(AllowedHosts::empty()), asker()).unwrap();
    let hosts = proxy.write_hosts("hosts", map).unwrap();
    let p = Profile {
        checkout: &checkout,
        scratch: &scratch,
        proxy_dir: proxy.dir(),
        extra_env: &[],
        ro_binds: &[],
        hosts_file: &hosts,
        tunnels: map,
    };
    let program: Vec<String> = program.iter().map(|s| s.to_string()).collect();
    let args = bwrap_args(&p, &program).unwrap();
    let out = std::process::Command::new(ap_core::sandbox::bwrap_path().unwrap())
        .env_clear()
        .args(&args)
        .output()
        .unwrap();
    (out, proxy.refusals())
}

/// The sandbox reads the mapped hosts file as its `/etc/hosts`.
#[test]
fn the_sandbox_reads_the_mapped_hosts_file() {
    if let Some(why) = super::tests::no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let map = bridge_addresses(
        &["github.com".to_string()],
        &AllowedHosts::empty(),
        &asker(),
    )
    .unwrap();
    let (out, _) = run_mapped(&map, &["cat", "/etc/hosts"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "127.0.0.1 localhost\n::1 localhost\n127.0.0.2 github.com\n"
    );
}

/// `localhost` resolves in the sandbox, as it does on the host.
#[test]
fn localhost_resolves_in_the_sandbox() {
    if let Some(why) = super::tests::no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let script = "import socket; print(socket.gethostbyname('localhost'))";
    let (out, _) = run_mapped(&[], &[PYTHON, "-I", "-c", script]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "127.0.0.1\n");
}

/// A client that connects to a mapped name on 443 reaches the proxy as a
/// `CONNECT` for that name. The name does not resolve, so the proxy refuses it
/// with the name in the refusal, with or without the network.
#[test]
fn a_mapped_name_reaches_the_proxy_as_connect_on_443() {
    if let Some(why) = super::tests::no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let map = bridge_addresses(
        &["nonexistent.github.com".to_string()],
        &AllowedHosts::empty(),
        &asker(),
    )
    .unwrap();
    let script = "import socket; s = socket.create_connection(('nonexistent.github.com', 443), timeout=60); print(repr(s.recv(1)))";
    let (out, refused) = run_mapped(&map, &[PYTHON, "-I", "-c", script]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b''\n");
    assert!(
        refused
            .iter()
            .any(|r| r.contains("nonexistent.github.com:443")),
        "{refused:?}"
    );
}

/// A connection to an address with no mapped name is refused by the proxy,
/// and the refusal names the address.
#[test]
fn an_unmapped_address_is_refused_and_logged() {
    if let Some(why) = super::tests::no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let script = "import socket; s = socket.create_connection(('127.0.0.9', 443), timeout=60); print(repr(s.recv(1)))";
    let (out, refused) = run_mapped(&[], &[PYTHON, "-I", "-c", script]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b''\n");
    assert!(
        refused.iter().any(|r| r.contains("127.0.0.9:443")),
        "{refused:?}"
    );
}

/// A name with no mapping does not resolve in the sandbox.
#[test]
fn an_unmapped_name_does_not_resolve() {
    if let Some(why) = super::tests::no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let script = "import socket\ntry:\n    socket.getaddrinfo('example.org', 443)\n    print('resolved')\nexcept socket.gaierror:\n    print('refused')";
    let (out, _) = run_mapped(&[], &[PYTHON, "-I", "-c", script]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "refused\n");
}
