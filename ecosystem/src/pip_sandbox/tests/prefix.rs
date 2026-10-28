use super::*;
use std::fs;

#[test]
fn a_program_under_bin_is_not_refused_as_a_prefix_of_root() {
    // `/bin/<x>` has the prefix `/`, which holds the home directory. The
    // file is in the system directories, so nothing is bound for it.
    assert_eq!(interpreter_prefix(Path::new("/bin/pip")), None);
    assert_eq!(interpreter_prefix(Path::new("/usr/bin/python3")), None);
    assert_eq!(
        interpreter_prefix(Path::new("/opt/py/bin/python")),
        Some(PathBuf::from("/opt/py"))
    );
    // A file directly under `/` outside the system dirs is still refused.
    let f = fixture();
    let prefix = interpreter_prefix(Path::new("/x")).unwrap();
    assert!(check_prefix("x", &prefix, &f.home).is_err());
    if Path::new("/bin/sh").is_file() {
        let mut f = fixture();
        f.host.path = "/bin:/usr/bin".into();
        let args = bwrap_args(&["/bin/sh"], &f.host).expect("/bin/sh is sandboxed");
        assert!(args.iter().any(|a| a == "/bin/sh"));
    }
}

#[test]
fn a_pip_under_a_credential_directory_is_refused_with_advice() {
    let mut f = fixture();
    let bin = f.home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("pip"), "#!/usr/bin/python3\n").unwrap();
    f.host.path = bin.into_os_string();
    let err = bwrap_args(&["pip"], &f.host).unwrap_err().to_string();
    assert!(
        err.contains(".local") && err.contains("--unsafe-resolve"),
        "{err}"
    );
}

#[test]
fn a_prefix_that_contains_the_home_is_refused() {
    let f = fixture();
    assert!(sensitive_reason(Path::new("/"), &f.home).is_some());
    assert!(sensitive_reason(f.home.parent().unwrap(), &f.home).is_some());
    assert!(sensitive_reason(&f.home, &f.home).is_some());
    assert!(sensitive_reason(&f.home.join(".ssh"), &f.home).is_some());
    assert!(sensitive_reason(&f.home.join(".gnupg/private-keys-v1.d"), &f.home).is_some());
    assert!(sensitive_reason(&f.home.join("venv"), &f.home).is_none());
    assert!(sensitive_reason(&f.home.join(".local/share/pipx/venvs/x"), &f.home).is_none());
    assert!(sensitive_reason(Path::new("/opt/python"), &f.home).is_none());
}

#[test]
fn prefixes_under_config_cache_or_local_are_refused_unless_a_venv() {
    let f = fixture();
    let h = &f.home;
    // The home-level directories themselves are refused even as venvs.
    for dir in [".local", ".config", ".cache"] {
        let p = h.join(dir);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert!(check_prefix("tool", &p, h).is_err(), "{dir} accepted");
    }
    for dir in [
        ".local/share",
        ".local/share/pipx/venvs/x",
        ".config/myapp",
        ".cache/tool",
    ] {
        let p = h.join(dir);
        assert!(check_prefix("tool", &p, h).is_err(), "{dir} accepted");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert!(check_prefix("tool", &p, h).is_ok(), "venv {dir} refused");
    }
    assert!(check_prefix("tool", &h.join("venv"), h).is_ok());
    assert!(check_prefix("tool", Path::new("/opt/python"), h).is_ok());
}

#[test]
fn a_standalone_interpreter_tree_under_local_is_accepted() {
    let f = fixture();
    let h = &f.home;
    // A uv-managed build: no pyvenv.cfg, an interpreter and a stdlib.
    let uv = h.join(".local/share/uv/python/cpython-3.12");
    fs::create_dir_all(uv.join("bin")).unwrap();
    fs::write(uv.join("bin/python3"), "").unwrap();
    fs::create_dir_all(uv.join("lib/python3.12")).unwrap();
    assert!(check_prefix("pip", &uv, h).is_ok());
    // The home-level directories stay refused even with an interpreter.
    let local = h.join(".local");
    fs::create_dir_all(local.join("bin")).unwrap();
    fs::write(local.join("bin/python3"), "").unwrap();
    fs::create_dir_all(local.join("lib/python3.12")).unwrap();
    assert!(check_prefix("pip", &local, h).is_err());
}

#[test]
fn a_config_dir_with_a_bin_but_no_python_library_is_refused() {
    let f = fixture();
    let h = &f.home;
    // Like ~/.config/gh: a bin/ directory, but no lib/python3* tree.
    let gh = h.join(".config/gh");
    fs::create_dir_all(gh.join("bin")).unwrap();
    fs::write(gh.join("bin/gh"), "").unwrap();
    assert!(check_prefix("pip", &gh, h).is_err());
    // An interpreter alone, without a stdlib directory, is not enough.
    fs::write(gh.join("bin/python3"), "").unwrap();
    assert!(check_prefix("pip", &gh, h).is_err());
    // A stdlib alone, without an interpreter, is not enough either.
    let cache = h.join(".cache/tool");
    fs::create_dir_all(cache.join("lib/python3.12")).unwrap();
    assert!(check_prefix("pip", &cache, h).is_err());
}

#[test]
fn a_pip_config_file_naming_credentials_is_not_bound() {
    let mut f = fixture();
    let key = f.home.join(".ssh/id_ed25519");
    f.host
        .vars
        .push(("PIP_CONFIG_FILE".into(), key.clone().into()));
    let args = build(&f);
    assert!(
        !binds(&args)
            .iter()
            .any(|(_, s, _)| s == &key.to_string_lossy())
    );
    assert!(!setenvs(&args).iter().any(|(k, _)| k == "PIP_CONFIG_FILE"));
}

#[test]
fn a_pip_config_file_under_the_config_dir_is_bound_but_the_dir_itself_is_not() {
    let mut f = fixture();
    let conf = f.home.join(".config/pip/pip.conf");
    fs::write(&conf, "[global]\n").unwrap();
    f.host
        .vars
        .push(("PIP_CONFIG_FILE".into(), conf.clone().into()));
    let b = binds(&build(&f));
    assert!(b.iter().any(|(_, s, _)| s == &conf.to_string_lossy()));
    assert!(sensitive_file(&f.home.join(".config/pip/pip.conf"), &f.home).is_none());
    assert!(sensitive_file(&f.home.join(".config"), &f.home).is_some());
}

#[test]
fn a_missing_program_is_reported_as_not_found() {
    let f = fixture();
    let err = bwrap_args(&["python3.99", "-m", "pip"], &f.host).unwrap_err();
    assert!(matches!(err, SandboxError::ProgramNotFound(p) if p == "python3.99"));
}

#[test]
fn a_venv_interpreter_symlinked_into_the_system_still_binds_the_venv() {
    let f = fixture();
    let bin = f.home.join("venv/bin");
    std::os::unix::fs::symlink("/usr/bin/python3", bin.join("python3")).unwrap();
    let mut host = Host {
        vars: f.host.vars.clone(),
        ..f.host
    };
    host.path = bin.clone().into_os_string();
    let args = strs(&bwrap_args(&["python3", "-m", "pip"], &host).unwrap());
    let venv = f.home.join("venv").to_string_lossy().into_owned();
    // The venv's interpreter directory is bound, the venv itself is not.
    let bin = format!("{venv}/bin");
    let b = binds(&args);
    assert!(b.contains(&("--ro-bind".into(), bin.clone(), bin)));
    assert!(!b.iter().any(|(_, s, _)| s == &venv));
}

/// `~/tools` with `bin/pip` and a `.netrc` at its top level: the prefix
/// holds a credential, so it is refused, whatever the name.
#[test]
fn a_prefix_with_a_credential_at_its_top_level_is_refused() {
    let mut f = fixture();
    let tools = f.home.join("tools");
    fs::create_dir_all(tools.join("bin")).unwrap();
    fs::write(tools.join("bin/pip"), "#!/usr/bin/python3\n").unwrap();
    f.host.path = tools.join("bin").into_os_string();
    let files = [
        ".netrc",
        ".pypirc",
        ".env",
        ".env.prod",
        ".git-credentials",
        "id_ed25519",
        "deploy.pem",
        "credentials",
        "credentials.json",
    ];
    for name in files {
        fs::write(tools.join(name), "secret").unwrap();
        let err = bwrap_args(&["pip"], &f.host).unwrap_err();
        assert!(
            matches!(err, SandboxError::UnsafePrefix { .. })
                && err.to_string().contains("top level"),
            "{name}: {err}"
        );
        fs::remove_file(tools.join(name)).unwrap();
    }
    for dir in [".ssh", ".aws"] {
        fs::create_dir_all(tools.join(dir)).unwrap();
        let err = bwrap_args(&["pip"], &f.host).unwrap_err();
        assert!(
            matches!(err, SandboxError::UnsafePrefix { .. }),
            "{dir}: {err}"
        );
        fs::remove_dir(tools.join(dir)).unwrap();
    }
    // Once the credential is gone the prefix is accepted again.
    assert!(bwrap_args(&["pip"], &f.host).is_ok());
}

/// A prefix in the home directory is not bound whole: only its
/// interpreter directories are, and the user's own files stay out.
#[test]
fn a_prefix_in_the_home_binds_only_its_interpreter_parts() {
    let mut f = fixture();
    let tools = f.home.join("tools");
    for d in [
        "bin",
        "lib/python3.12/site-packages",
        "include/python3.12",
        "share/python3.12",
        "share/doc/tool",
        "etc",
    ] {
        fs::create_dir_all(tools.join(d)).unwrap();
    }
    fs::write(tools.join("bin/pip"), "#!/usr/bin/python3\n").unwrap();
    fs::write(tools.join("notes.txt"), "mine").unwrap();
    f.host.path = tools.join("bin").into_os_string();

    let args = strs(&bwrap_args(&["pip"], &f.host).unwrap());
    let tools_s = tools.to_string_lossy().into_owned();
    let mut ours: Vec<String> = binds(&args)
        .into_iter()
        .filter(|(_, s, _)| s.starts_with(&tools_s))
        .map(|(flag, s, d)| {
            assert_eq!(flag, "--ro-bind");
            assert_eq!(s, d);
            s
        })
        .collect();
    ours.sort();
    let expect: Vec<String> = ["bin", "include", "lib", "share/python3.12"]
        .iter()
        .map(|d| format!("{tools_s}/{d}"))
        .collect();
    assert_eq!(ours, expect);
    assert!(
        !args
            .iter()
            .any(|a| a.ends_with("notes.txt") || a.ends_with("share/doc/tool"))
    );
}

/// A venv with its `pyvenv.cfg` binds `bin`, `lib` and the config file, and
/// not the venv directory itself.
#[test]
fn a_venv_with_a_pyvenv_cfg_still_works() {
    let f = fixture();
    let venv = f.home.join("venv");
    fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
    fs::create_dir_all(venv.join("lib/python3.12/site-packages")).unwrap();
    let args = build(&f);
    let b = binds(&args);
    let v = venv.to_string_lossy().into_owned();
    for d in [
        format!("{v}/bin"),
        format!("{v}/lib"),
        format!("{v}/pyvenv.cfg"),
    ] {
        assert!(
            b.contains(&("--ro-bind".into(), d.clone(), d.clone())),
            "{d} not bound: {b:?}"
        );
    }
    assert!(!b.iter().any(|(_, s, _)| s == &v));
    let path = setenvs(&args)
        .into_iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v)
        .unwrap();
    assert!(path.starts_with(&format!("{v}/bin:")), "{path}");
}

#[test]
fn shebang_interpreters_are_found_through_env() {
    let f = fixture();
    let script = f.home.join("venv/bin/tool");
    fs::write(&script, "#!/usr/bin/env python3\n").unwrap();
    let found = shebang_interpreter(&script, OsStr::new("/usr/bin"));
    assert_eq!(found, Some(PathBuf::from("/usr/bin/python3")));
    fs::write(&script, "#!/opt/py/bin/python3 -I\n").unwrap();
    assert_eq!(
        shebang_interpreter(&script, OsStr::new("/usr/bin")),
        Some(PathBuf::from("/opt/py/bin/python3"))
    );
}
