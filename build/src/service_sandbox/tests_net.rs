use super::tests::{no_sandbox, test_asker};
use super::*;

/// The device nodes and links the sandbox's `/dev` is built from.
#[test]
fn the_sandbox_dev_has_only_the_device_nodes_it_needs() {
    let args = dev_args();
    let binds: Vec<&str> = args
        .windows(3)
        .filter(|w| w[0] == "--dev-bind-try")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(
        binds,
        [
            "/dev/null",
            "/dev/zero",
            "/dev/full",
            "/dev/random",
            "/dev/urandom",
            "/dev/tty"
        ]
    );
    assert!(!args.iter().any(|a| a == "--dev" || a == "--dev-bind"));
    assert_eq!(args[..2], ["--tmpfs", "/dev"]);
    let p = Profile {
        checkout: Path::new("/work/proj/pkg"),
        scratch: Path::new("/work/proj/pkg/.ap-service-x"),
        proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
        extra_env: &[],
        ro_binds: &[],
        hosts_file: Path::new("/tmp/ap-service-proxy-x/hosts"),
        tunnels: &[],
    };
    let bwrap = bwrap_args(&p, &["true".to_string()]).unwrap();
    assert!(
        bwrap
            .windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == "/dev")
    );
    assert!(!bwrap.iter().any(|a| a == "--dev"));
}

/// Run `program` in the sandbox with a fresh checkout; the outcome.
pub(super) fn run_sandboxed(program: &[&str]) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    let checkout = fs::canonicalize(root.path()).unwrap();
    let scratch = checkout.join(".ap-service-t");
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.join(sub)).unwrap();
    }
    let proxy = Proxy::start(Arc::new(AllowedHosts::empty()), test_asker()).unwrap();
    let hosts = proxy.write_hosts("hosts", &[]).unwrap();
    let p = Profile {
        checkout: &checkout,
        scratch: &scratch,
        proxy_dir: proxy.dir(),
        extra_env: &[],
        ro_binds: &[],
        hosts_file: &hosts,
        tunnels: &[],
    };
    let program: Vec<String> = program.iter().map(|s| s.to_string()).collect();
    let args = bwrap_args(&p, &program).unwrap();
    std::process::Command::new(ap_core::sandbox::bwrap_path().unwrap())
        .env_clear()
        .args(&args)
        .output()
        .unwrap()
}

#[test]
fn the_sandbox_dev_works_for_programs() {
    if let Some(why) = no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let script = "echo ok > /dev/null && head -c 4 /dev/urandom | wc -c && ls /dev/pts /dev/ptmx 2>/dev/null; true";
    let out = run_sandboxed(&["/bin/sh", "-c", script]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("4\n"), "{stdout}");
    assert!(
        !stdout.contains("ptmx") && !stdout.contains("pts"),
        "{stdout}"
    );
}

/// A service starts with no inheritable, permitted, effective or ambient
/// capabilities, and with no_new_privs set, so it cannot regain what the
/// bridge holds.
#[test]
fn the_service_starts_without_capabilities() {
    if let Some(why) = no_sandbox() {
        eprintln!("skipped: {why}");
        return;
    }
    let out = run_sandboxed(&[
        "grep",
        "-E",
        "^(CapInh|CapPrm|CapEff|CapAmb|NoNewPrivs)",
        "/proc/self/status",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "CapInh:\t0000000000000000\nCapPrm:\t0000000000000000\nCapEff:\t0000000000000000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\n"
    );
}

/// Probe, with the network: the jay `_service` in URL form through
/// `run_services`, which runs the real `cargo_vendor` helper in the sandbox.
/// The checkout is a directory named `jay` with a minimal `jay.spec`: the
/// helper looks for `<directory>.spec` in the checkout, and an unnamed
/// directory fails with `Specfile not found`. Ignored by default; run with
/// `cargo test -p ap-build probe_cargo_vendor_url_form -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe_cargo_vendor_url_form_in_the_sandbox() {
    if let Some(why) = no_sandbox() {
        panic!("cannot probe here: {why}");
    }
    let root = tempfile::tempdir().unwrap();
    // The helper names its outputs after the checkout directory, so the spec
    // file is `<directory>.spec`.
    let checkout_buf = root.path().join("jay");
    fs::create_dir(&checkout_buf).unwrap();
    let checkout = checkout_buf.as_path();
    fs::write(
        checkout.join("jay.spec"),
        "Name: jay\nVersion: 1.9.1\nRelease: 0\nSummary: probe\nLicense: GPL-3.0-only\n",
    )
    .unwrap();
    let params = [
        ("url", "https://github.com/mahkoh/jay"),
        ("revision", "v1.9.1"),
        ("compression", "zst"),
        ("update", "true"),
        ("versionrewriteregex", "^v?(.*)"),
        ("versionrewritepattern", "$1"),
        ("changesgenerate", "true"),
        ("changesauthor", "probe"),
        ("changesemail", "probe@example.org"),
    ];
    let services = [ServiceCall {
        name: "cargo_vendor".to_string(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }];
    let out = run_services(checkout, "jay", &services, Emit::quiet()).unwrap();
    let mut names: Vec<String> = fs::read_dir(checkout)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    eprintln!(
        "probe exit {}: {}\n{}",
        out.code,
        out.stderr,
        names.join("\n")
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    for want in ["jay-1.9.1.tar.zst", "vendor.tar.zst"] {
        assert!(names.iter().any(|n| n == want), "{names:?}");
    }
}
