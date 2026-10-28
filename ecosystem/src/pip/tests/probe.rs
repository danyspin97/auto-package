use super::*;

#[test]
fn unset_xdg_config_dirs_default_to_etc_xdg() {
    let got = config_candidates(&BTreeMap::new(), None);
    assert_eq!(
        got[8],
        Some(std::path::PathBuf::from("/etc/xdg/pip/pip.conf"))
    );
    assert_eq!(
        got.last(),
        Some(&None),
        "no prefix, so no <prefix>/pip.conf"
    );
    assert_eq!(got.len(), 10, "{got:?}");
}

#[test]
fn the_interpreter_prefix_pip_conf_changes_the_cache_key() {
    let dir = tempfile::tempdir().unwrap();
    let venv = dir.path();
    let key = |body: Option<&str>| {
        if let Some(body) = body {
            std::fs::write(venv.join("pip.conf"), body).unwrap();
        }
        cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            Some(venv),
        )
    };
    let without = key(None);
    let with = key(Some("[global]\nindex-url = https://a.invalid/simple\n"));
    assert_ne!(without, with, "<prefix>/pip.conf must change the cache key");
}

#[test]
fn interpreter_prefix_is_found_on_path_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("venv/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("pip"), "").unwrap();
    let path = bin.clone().into_os_string();
    assert_eq!(
        interpreter_prefix_of("pip", &path),
        Some(dir.path().join("venv"))
    );
    assert_eq!(interpreter_prefix_of("missing-pip", &path), None);
    assert_eq!(
        interpreter_prefix_of("/opt/py/bin/python3", OsStr::new("")),
        Some(std::path::PathBuf::from("/opt/py"))
    );
    assert_eq!(
        interpreter_prefix_of("/opt/tool/pip", OsStr::new("")),
        Some(std::path::PathBuf::from("/opt/tool"))
    );
}

#[test]
fn pip_version_names_the_interpreter_it_runs_under() {
    for (out, want) in [
        (
            "pip 24.0 from /usr/lib/python3/dist-packages/pip (python 3.12)\n",
            "Python 3.12",
        ),
        (
            "pip 23.3.1 from /home/u/.local/lib/python3.11/site-packages/pip (python 3.11)\n",
            "Python 3.11",
        ),
        (
            "pip 26.2.1 from /usr/lib/python3.13/site-packages/pip (python 3.13)\n",
            "Python 3.13",
        ),
        (
            "pip 25.1 from /opt/py/lib/python3.10/site-packages/pip (python 3.10.12)\n",
            "Python 3.10",
        ),
    ] {
        assert_eq!(parse_pip_version(out).as_deref(), Some(want), "{out}");
    }
}

#[test]
fn pip_version_without_a_python_version_is_not_parsed() {
    assert_eq!(parse_pip_version(""), None);
    assert_eq!(parse_pip_version("pip 24.0 from /usr/lib/pip\n"), None);
    assert_eq!(parse_pip_version("pip 24.0 from /x (python 3)\n"), None);
    assert_eq!(parse_pip_version("pip 24.0 from /x (python x.y)\n"), None);
}

#[test]
fn run_with_timeout_kills_a_slow_child() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let t = Instant::now();
    let err = run_with_timeout(cmd, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(err, Error::Failed { .. }), "{err}");
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn run_with_timeout_collects_output() {
    let mut cmd = Command::new("echo");
    cmd.arg("hi");
    let out = run_with_timeout(cmd, Duration::from_secs(10)).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
}

#[test]
fn run_with_timeout_does_not_wait_for_a_leftover_grandchild() {
    // The shell exits at once but its background `sleep` keeps the pipes
    // open; the group kill after exit must let the readers finish.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30 & echo hi"]);
    let t = Instant::now();
    let out = run_with_timeout(cmd, Duration::from_secs(10)).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn run_with_timeout_kills_grandchildren_holding_the_pipes() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30 & wait"]);
    let t = Instant::now();
    let err = run_with_timeout(cmd, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(err, Error::Failed { .. }), "{err}");
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn cache_key_differs_from_the_old_narrow_scheme() {
    // Old key was `<name>-<version>.json`. The new key must never match it,
    // so stale entries from before this change are simply ignored (the TTL
    // in load_cached_resolve prunes them).
    let old = "httpx-0.28.1.json";
    let new = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_ne!(old, new);
}
