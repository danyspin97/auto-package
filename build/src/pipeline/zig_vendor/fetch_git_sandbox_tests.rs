//! The sandboxed git: the environment it runs in, and what it can reach. These
//! run bubblewrap, and skip with a message where it cannot run, as the
//! `service_sandbox` tests do. None needs the network; the probe at the end does,
//! and is ignored by default.

use super::fetch_git::{self, GitRun, fetch_commit};
use super::fetch_git_sandbox::SandboxGit;
use super::unpack::LIMITS;

const HOST: &str = "gitlab.freedesktop.org";
const HEX40: &str = "c183af18ae35dd5fb3dde709697c8f84bb1dc7f3";

/// Whether bubblewrap can run here; prints why not when it cannot.
fn sandbox_here() -> bool {
    match crate::service_sandbox::sandbox_ready() {
        Ok(_) => true,
        Err(why) => {
            eprintln!("skipped: the sandbox cannot run here: {why}");
            false
        }
    }
}

/// Run `args` in the sandbox for `host`, in a fresh directory, with the limit
/// a fetch gets.
fn run(host: &str, args: &[&str]) -> Result<String, String> {
    run_capped(host, LIMITS.max_bytes, args)
}

/// [`run`] with a file size limit of `max_file` bytes.
fn run_capped(host: &str, max_file: u64, args: &[&str]) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    SandboxGit { max_file }.run("pkg", host, dir.path(), &args)
}

#[test]
fn git_runs_with_an_empty_home_and_no_system_or_global_config() {
    if !sandbox_here() {
        return;
    }
    let out = run(
        HOST,
        &[
            "/bin/sh",
            "-c",
            "echo \"home=$HOME\"; [ -z \"$(ls -A \"$HOME\")\" ] && echo empty; \
             /usr/bin/git config --list --show-origin | cut -f1 | sort -u",
        ],
    )
    .unwrap();
    assert!(out.contains("home=/tmp/home\n"), "{out}");
    assert!(out.contains("empty\n"), "{out}");
    // Only the command line's entries: no system, global or local file.
    assert!(!out.contains("file:"), "{out}");
}

#[test]
fn git_refuses_every_protocol_but_https() {
    if !sandbox_here() {
        return;
    }
    let err = run(HOST, &["/usr/bin/git", "ls-remote", "file:///etc/hostname"]).unwrap_err();
    assert!(err.contains("not allowed"), "{err}");
}

#[test]
fn git_cannot_reach_a_host_off_the_allowlist() {
    if !sandbox_here() {
        return;
    }
    let host = "evil.example.net";
    let err = run(
        host,
        &["/usr/bin/git", "ls-remote", "https://evil.example.net/o/r"],
    )
    .unwrap_err();
    assert!(err.contains("--allow-host evil.example.net"), "{err}");
}

#[test]
fn git_reaches_only_the_dependency_host_not_another_allowed_one() {
    if !sandbox_here() {
        return;
    }
    // github.com is allowed for services, but this dependency is on the other
    // host: the proxy of this command refuses it before any connection.
    let err = run(
        HOST,
        &["/usr/bin/git", "ls-remote", "https://github.com/o/r"],
    )
    .unwrap_err();
    assert!(err.contains("sandbox proxy refused: 403"), "{err}");
}

#[test]
fn a_file_the_sandbox_writes_past_the_limit_fails() {
    if !sandbox_here() {
        return;
    }
    let script = "head -c 512 /dev/zero > small && head -c 4096 /dev/zero > big";
    // A write past the limit kills the command with SIGXFSZ, so it may print nothing.
    assert!(run_capped(HOST, 1024, &["/bin/sh", "-c", script]).is_err());
    let ok = run_capped(
        HOST,
        1024,
        &["/bin/sh", "-c", "head -c 512 /dev/zero > small"],
    );
    assert!(ok.is_ok(), "{ok:?}");
}

/// CI sets `AP_REQUIRE_SANDBOX_TESTS=1` where bubblewrap must work: there the
/// tests above must run, not return early, so a missing sandbox fails here.
#[test]
fn the_sandbox_tests_ran_or_say_why() {
    if std::env::var_os("AP_REQUIRE_SANDBOX_TESTS").is_some_and(|v| v == "1") {
        if let Err(why) = crate::service_sandbox::sandbox_ready() {
            panic!("AP_REQUIRE_SANDBOX_TESTS=1, but the sandbox cannot run: {why}");
        }
    }
}

#[test]
fn the_command_is_limited_by_prlimit_and_prlimit_accepts_the_limits() {
    let args = super::fetch_git_sandbox::prlimit_args(1024);
    assert_eq!(
        args,
        [
            "--fsize=1024",
            "--nofile=4096",
            "--as=4294967296",
            "--cpu=600",
            "--",
        ]
    );
    if !std::path::Path::new("/usr/bin/prlimit").is_file() {
        eprintln!("skipped: /usr/bin/prlimit is not installed");
        return;
    }
    // Run `prlimit` with the limits, and have a second `prlimit` print them.
    let out = std::process::Command::new("/usr/bin/prlimit")
        .args(&args)
        .args([
            "/usr/bin/prlimit",
            "--noheadings",
            "--output=RESOURCE,SOFT",
            "--fsize",
            "--nofile",
            "--as",
            "--cpu",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // The columns are padded, so each row is compared as its two fields.
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    for (name, value) in [
        ("FSIZE", "1024"),
        ("NOFILE", "4096"),
        ("AS", "4294967296"),
        ("CPU", "600"),
    ] {
        assert!(
            rows.iter().any(|r| r[..] == [name, value]),
            "{name} {value} not in:\n{text}"
        );
    }
}

#[test]
fn the_sandbox_has_no_network_interface_but_loopback() {
    if !sandbox_here() {
        return;
    }
    let out = run(
        HOST,
        &[
            "/bin/sh",
            "-c",
            "tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' '",
        ],
    )
    .unwrap();
    assert_eq!(out, "lo\n");
}

/// A fetch of a commit that does not exist, through the sandbox. The probe needs
/// the network: it reaches the real host through the proxy. Run it with
/// `cargo test -p ap-build fetch_git_sandbox -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe_a_missing_commit_is_refused_through_the_sandbox() {
    if !sandbox_here() {
        panic!("cannot probe here: the sandbox cannot run");
    }
    let dir = tempfile::tempdir().unwrap();
    let src = fetch_git::parse(&format!("{HOST}/ifreund/zig-pixman#{HEX40}"), "u").unwrap();
    let git = SandboxGit {
        max_file: LIMITS.max_bytes,
    };
    let err = fetch_commit(&git, "pkg", &src, dir.path(), LIMITS).unwrap_err();
    eprintln!("probe: {err}");
    assert!(err.contains(HEX40), "{err}");
}
