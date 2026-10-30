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
    let p = Profile {
        checkout: &checkout,
        scratch: &scratch,
        proxy_dir: proxy.dir(),
        extra_env: &[],
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
