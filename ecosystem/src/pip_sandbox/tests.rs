use super::*;
use std::fs;

mod ca;
mod prefix;

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    host: Host,
}

/// A fake host: a home directory with a canary and credentials in it, and
/// a venv with a pip script.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("home/u");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::create_dir_all(home.join(".config/pip")).unwrap();
    fs::write(home.join(".ssh/id_ed25519"), "secret").unwrap();
    fs::write(home.join("canary"), "canary").unwrap();
    fs::write(home.join(".netrc"), "machine x password y").unwrap();
    let venv = home.join("venv");
    fs::create_dir_all(venv.join("bin")).unwrap();
    fs::write(venv.join("bin/pip"), "#!/usr/bin/python3\n").unwrap();
    let host = Host {
        home: home.clone(),
        path: format!("{}:/usr/bin", venv.join("bin").display()).into(),
        vars: vec![
            ("LANG".into(), "C.UTF-8".into()),
            ("HOME".into(), home.clone().into()),
            ("HTTPS_PROXY".into(), "http://proxy.invalid:3128".into()),
        ],
        netrc: false,
    };
    Fixture {
        _dir: dir,
        home,
        host,
    }
}

fn strs(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// `(flag, src, dst)` triples of every bind in `args`.
fn binds(args: &[String]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if matches!(
            args[i].as_str(),
            "--bind" | "--ro-bind" | "--bind-try" | "--ro-bind-try"
        ) {
            out.push((args[i].clone(), args[i + 1].clone(), args[i + 2].clone()));
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

fn setenvs(args: &[String]) -> Vec<(String, String)> {
    args.windows(3)
        .filter(|w| w[0] == "--setenv")
        .map(|w| (w[1].clone(), w[2].clone()))
        .collect()
}

fn build(f: &Fixture) -> Vec<String> {
    strs(&bwrap_args(&["pip", "install"], &f.host).unwrap())
}

#[test]
fn network_is_shared_and_everything_else_is_unshared() {
    let args = build(&fixture());
    let pos = |s: &str| args.iter().position(|a| a == s);
    assert!(pos("--unshare-all").unwrap() < pos("--share-net").unwrap());
    assert!(!args.iter().any(|a| a == "--unshare-net"));
    for flag in ["--die-with-parent", "--new-session", "--clearenv"] {
        assert!(pos(flag).is_some(), "{flag} missing from {args:?}");
    }
}

#[test]
fn the_root_is_remounted_read_only_after_the_last_mount() {
    let args = build(&fixture());
    let ro = args.iter().position(|a| a == "--remount-ro").unwrap();
    assert_eq!(args[ro + 1], "/");
    let last_mount = args
        .iter()
        .rposition(|a| a.starts_with("--ro-bind") || a == "--tmpfs" || a == "--dir")
        .unwrap();
    assert!(last_mount < ro, "{args:?}");
}

#[test]
fn the_real_home_is_not_bound_beyond_the_venv() {
    let f = fixture();
    let args = build(&f);
    let home = f.home.to_string_lossy().into_owned();
    let venv = format!("{home}/venv");
    for (flag, src, dst) in binds(&args) {
        assert!(flag.starts_with("--ro-bind"), "writable bind: {flag} {src}");
        assert!(
            dst.starts_with(&format!("{venv}/")) || dst == venv || !dst.starts_with(&home),
            "{dst} is under the real home: {args:?}"
        );
        if src.starts_with(&home) {
            assert!(
                src.starts_with(&format!("{venv}/bin")),
                "unexpected host path bound: {src}"
            );
        }
    }
    // The sandbox home is an empty tmpfs.
    let tmpfs: Vec<_> = args
        .windows(2)
        .filter(|w| w[0] == "--tmpfs")
        .map(|w| w[1].as_str())
        .collect();
    for dir in ["/tmp", "/run", SANDBOX_HOME] {
        assert!(tmpfs.contains(&dir), "{dir} not a tmpfs: {tmpfs:?}");
    }
    assert!(
        !args
            .iter()
            .any(|a| a.contains(".ssh") || a.contains("canary"))
    );
}

#[test]
fn only_listed_system_paths_are_bound_and_all_are_read_only() {
    let f = fixture();
    let args = build(&f);
    let venv = format!("{}/venv", f.home.display());
    for (flag, src, dst) in binds(&args) {
        assert!(flag.starts_with("--ro-bind"), "{flag} {src}");
        let known = ["/usr", "/bin", "/lib", "/lib64", "/sbin"].contains(&src.as_str())
            || ETC_ENTRIES.contains(&src.as_str())
            || src.starts_with(&format!("{venv}/"));
        assert!(known, "unexpected bind {src} -> {dst}");
    }
    for never in ["/", "/etc", "/home", "/var", "/run", "/root", "/opt"] {
        assert!(
            !binds(&args).iter().any(|(_, s, _)| s == never),
            "{never} must not be bound"
        );
    }
}

#[test]
fn environment_is_cleared_then_set_from_the_allowlist_and_sandbox_paths() {
    let mut f = fixture();
    f.host
        .vars
        .push(("XDG_CONFIG_HOME".into(), f.home.join(".config").into()));
    f.host.vars.push(("TMPDIR".into(), "/home/u/tmp".into()));
    let args = build(&f);
    let env = setenvs(&args);
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert!(args.contains(&"--clearenv".to_string()));
    assert_eq!(get("HOME"), Some("/home/sandbox"));
    assert_eq!(get("TMPDIR"), Some("/tmp"));
    assert_eq!(get("XDG_CACHE_HOME"), Some("/home/sandbox/.cache"));
    assert_eq!(get("XDG_CONFIG_HOME"), Some("/home/sandbox/.config"));
    assert_eq!(get("LANG"), Some("C.UTF-8"));
    assert_eq!(get("HTTPS_PROXY"), Some("http://proxy.invalid:3128"));
    let path = get("PATH").unwrap();
    assert!(
        path.starts_with(&format!("{}/venv/bin:", f.home.display())),
        "{path}"
    );
    assert!(path.ends_with("/usr/bin:/bin"), "{path}");
    // Every host value that names the real home is replaced.
    let home = f.home.to_string_lossy().into_owned();
    for (k, v) in &env {
        assert!(
            !v.contains(&home) || k == "PATH",
            "{k}={v} leaks the real home"
        );
    }
    // Each variable is set once.
    let names: BTreeSet<_> = env.iter().map(|(k, _)| k).collect();
    assert_eq!(names.len(), env.len(), "{env:?}");
}

#[test]
fn the_command_follows_the_options_and_keeps_pips_arguments() {
    let f = fixture();
    let args = build(&f);
    let n = args.len();
    assert_eq!(args[n - 3], "--");
    assert_eq!(args[n - 2], format!("{}/venv/bin/pip", f.home.display()));
    assert_eq!(args[n - 1], "install");
    let chdir = args.iter().position(|a| a == "--chdir").unwrap();
    assert_eq!(args[chdir + 1], "/tmp");
}

#[test]
fn pip_config_files_are_bound_one_by_one_and_netrc_is_opt_in() {
    let mut f = fixture();
    fs::write(f.home.join(".config/pip/pip.conf"), "[global]\n").unwrap();
    let args = build(&f);
    let b = binds(&args);
    let conf = f
        .home
        .join(".config/pip/pip.conf")
        .to_string_lossy()
        .into_owned();
    assert!(b.contains(&(
        "--ro-bind".into(),
        conf,
        "/home/sandbox/.config/pip/pip.conf".into()
    )));
    assert!(
        !b.iter().any(|(_, _, d)| d.ends_with(".netrc")),
        "netrc bound by default: {b:?}"
    );

    f.host.netrc = true;
    let b = binds(&build(&f));
    let netrc = f.home.join(".netrc").to_string_lossy().into_owned();
    assert!(b.contains(&("--ro-bind".into(), netrc, "/home/sandbox/.netrc".into())));
}

#[test]
fn pip_config_file_variable_is_remapped_into_the_sandbox() {
    let mut f = fixture();
    let cfg = f.home.join("private-pip.conf");
    fs::write(&cfg, "[global]\nindex-url = https://idx.invalid/simple\n").unwrap();
    f.host
        .vars
        .push(("PIP_CONFIG_FILE".into(), cfg.clone().into()));
    let args = build(&f);
    assert!(binds(&args).contains(&(
        "--ro-bind".into(),
        cfg.to_string_lossy().into_owned(),
        SANDBOX_PIP_CONFIG_FILE.into()
    )));
    assert!(setenvs(&args).contains(&("PIP_CONFIG_FILE".into(), SANDBOX_PIP_CONFIG_FILE.into())));

    f.host.vars.retain(|(k, _)| k != "PIP_CONFIG_FILE");
    f.host
        .vars
        .push(("PIP_CONFIG_FILE".into(), "/dev/null".into()));
    let args = build(&f);
    assert!(setenvs(&args).contains(&("PIP_CONFIG_FILE".into(), "/dev/null".into())));
}
