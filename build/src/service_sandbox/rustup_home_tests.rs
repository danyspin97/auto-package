use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

fn argv(binds: &[PathBuf], env: &[(String, String)]) -> Vec<String> {
    let p = Profile {
        checkout: Path::new("/work/proj/pkg"),
        scratch: Path::new("/work/proj/pkg/.ap-service-x"),
        proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
        extra_env: env,
        ro_binds: binds,
        hosts_file: Path::new("/tmp/ap-service-proxy-x/hosts"),
        tunnels: &[],
    };
    bwrap_args(&p, &["/usr/lib/obs/service/cargo_vendor".to_string()]).unwrap()
}

/// A temporary `$HOME` owned by the caller, with its uid.
struct Host {
    _root: tempfile::TempDir,
    home: PathBuf,
    uid: u32,
}

fn host() -> Host {
    let root = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(root.path()).unwrap().join("home");
    fs::create_dir(&home).unwrap();
    let uid = fs::metadata(&home).unwrap().uid();
    Host {
        _root: root,
        home,
        uid,
    }
}

impl Host {
    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.home.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// A rustup home at `rel` with a toolchain in it.
    fn rustup(&self, rel: &str) -> PathBuf {
        let p = self.dir(rel);
        fs::create_dir_all(p.join("toolchains/stable-x86_64-unknown-linux-gnu")).unwrap();
        p
    }

    fn vars(&self, rustup_home: Option<&Path>) -> HostVars {
        HostVars {
            rustup_home: rustup_home.map(|p| p.as_os_str().to_owned()),
            home: Some(self.home.clone()),
            cargo_home: None,
        }
    }

    fn resolve(&self, vars: &HostVars) -> RustupHome {
        resolve_rustup_home(vars, self.uid)
    }

    fn refused(&self, vars: &HostVars) -> String {
        match self.resolve(vars) {
            RustupHome::Refused(r) => r.why,
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_bound_rustup_home_appears_once_as_a_read_only_bind() {
    let binds = [PathBuf::from("/srv/rustup")];
    let env = [("RUSTUP_HOME".to_string(), "/srv/rustup".to_string())];
    let args = argv(&binds, &env);
    let found: Vec<_> = args
        .windows(3)
        .filter(|w| w[0] == "--ro-bind" && (w[1] == "/srv/rustup" || w[2] == "/srv/rustup"))
        .collect();
    assert_eq!(found.len(), 1, "{args:?}");
    assert_eq!(found[0], ["--ro-bind", "/srv/rustup", "/srv/rustup"]);
    let pos = |a: &str| args.iter().position(|x| x == a).unwrap();
    assert!(pos("--ro-bind") < pos("--clearenv"));
    assert!(
        args.windows(3)
            .any(|w| w == ["--setenv", "RUSTUP_HOME", "/srv/rustup"])
    );
    // CARGO_HOME stays in the scratch directory.
    assert!(args.windows(3).any(|w| w
        == [
            "--setenv",
            "CARGO_HOME",
            "/work/proj/pkg/.ap-service-x/cargo"
        ]));
}

#[test]
fn no_bind_means_nothing_of_the_home_or_cargo_in_the_argv() {
    let args = argv(&[], &[]);
    assert!(!args.iter().any(|a| a.contains(".rustup")
        || a.contains(".cargo")
        || a.contains("credentials")
        || a == "RUSTUP_HOME"));
    let home = std::env::var("HOME").unwrap_or_default();
    if home.len() > 1 {
        assert!(!args.iter().any(|a| a == &home));
    }
}

#[test]
fn a_bind_that_is_not_utf8_is_an_error_not_a_lossy_path() {
    let bad = PathBuf::from(std::ffi::OsStr::from_bytes(b"/srv/r\xff/.rustup"));
    let p = Profile {
        checkout: Path::new("/work/proj/pkg"),
        scratch: Path::new("/work/proj/pkg/.ap-service-x"),
        proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
        extra_env: &[],
        ro_binds: &[bad],
        hosts_file: Path::new("/tmp/ap-service-proxy-x/hosts"),
        tunnels: &[],
    };
    assert!(bwrap_args(&p, &["x".to_string()]).is_err());
}

#[test]
fn a_plain_rustup_home_is_accepted_at_its_canonical_path() {
    let h = host();
    let rustup = h.rustup(".rustup");
    assert_eq!(h.resolve(&h.vars(None)), RustupHome::Usable(rustup.clone()));
    // Named by RUSTUP_HOME, under any name, when it holds toolchains.
    let other = h.rustup("rust/home");
    assert_eq!(h.resolve(&h.vars(Some(&other))), RustupHome::Usable(other));
}

#[test]
fn nothing_set_up_is_absent_not_refused() {
    let h = host();
    assert_eq!(h.resolve(&h.vars(None)), RustupHome::Absent);
    assert_eq!(
        h.resolve(&h.vars(Some(&h.home.join("none")))),
        RustupHome::Absent
    );
    let no_home = HostVars::default();
    assert_eq!(h.resolve(&no_home), RustupHome::Absent);
}

#[test]
fn directories_that_are_not_a_rustup_home_are_refused() {
    let h = host();
    for name in [".ssh", ".gnupg", ".config"] {
        let dir = h.dir(name);
        fs::write(dir.join("secret"), "x").unwrap();
        let why = h.refused(&h.vars(Some(&dir)));
        assert!(why.contains("toolchains"), "{name}: {why}");
    }
    // Even with a toolchains directory inside, a name that is not .rustup
    // needs RUSTUP_HOME to name it directly: a `..` path does not.
    h.rustup(".ssh2");
    let sneaky = h.dir(".rustup").join("../.ssh2");
    fs::create_dir_all(h.dir(".rustup/x")).unwrap();
    let why = h.refused(&h.vars(Some(&sneaky)));
    assert!(why.contains("not named .rustup"), "{why}");
    assert!(matches!(
        resolve_rustup_home(
            &HostVars {
                rustup_home: Some("/etc".into()),
                ..h.vars(None)
            },
            h.uid
        ),
        RustupHome::Refused(_)
    ));
}

#[test]
fn a_rustup_link_to_another_directory_is_refused() {
    let h = host();
    let ssh = h.dir(".ssh");
    fs::create_dir_all(ssh.join("toolchains")).unwrap();
    symlink(&ssh, h.home.join(".rustup")).unwrap();
    let why = h.refused(&h.vars(None));
    assert!(why.contains("not named .rustup"), "{why}");
    // A link that leaves its directory.
    let far = h.dir("far/away/.rustup");
    fs::create_dir(far.join("toolchains")).unwrap();
    let link = h.dir("near").join("link");
    symlink(&far, &link).unwrap();
    let why = h.refused(&h.vars(Some(&link)));
    assert!(why.contains("link"), "{why}");
}

#[test]
fn without_home_a_rustup_home_is_refused() {
    let h = host();
    let vars = HostVars {
        rustup_home: Some(h.home.clone().into_os_string()),
        home: None,
        cargo_home: None,
    };
    fs::create_dir(h.home.join("toolchains")).unwrap();
    assert!(h.refused(&vars).contains("HOME is not set"));
}

#[test]
fn the_home_a_parent_and_the_cargo_home_are_refused() {
    let h = host();
    fs::create_dir(h.home.join("toolchains")).unwrap();
    assert!(h.refused(&h.vars(Some(&h.home))).contains("home directory"));
    let cargo = h.rustup(".cargo");
    assert!(h.refused(&h.vars(Some(&cargo))).contains("cargo home"));
    let same = h.rustup("both");
    let vars = HostVars {
        cargo_home: Some(same.clone().into_os_string()),
        ..h.vars(Some(&same))
    };
    assert!(h.refused(&vars).contains("cargo home"));
    // A directory that holds the cargo home.
    let outer = h.rustup("outer");
    let vars = HostVars {
        cargo_home: Some(outer.join("cargo").into_os_string()),
        ..h.vars(Some(&outer))
    };
    assert!(h.refused(&vars).contains("cargo home"));
}

#[test]
fn a_group_or_world_writable_directory_is_refused() {
    let h = host();
    let rustup = h.rustup(".rustup");
    for mode in [0o775, 0o757] {
        fs::set_permissions(&rustup, fs::Permissions::from_mode(mode)).unwrap();
        assert!(h.refused(&h.vars(None)).contains("writable"), "{mode:o}");
    }
}

#[test]
fn a_directory_without_toolchains_or_with_a_linked_one_is_refused() {
    let h = host();
    let rustup = h.dir(".rustup");
    assert!(h.refused(&h.vars(None)).contains("toolchains"));
    let target = h.dir("elsewhere");
    symlink(&target, rustup.join("toolchains")).unwrap();
    assert!(h.refused(&h.vars(None)).contains("toolchains"));
}

#[test]
fn another_owner_or_an_unsafe_parent_is_refused() {
    let h = host();
    h.rustup(".rustup");
    if h.uid != 0 {
        let vars = h.vars(None);
        let why = match resolve_rustup_home(&vars, h.uid + 1) {
            RustupHome::Refused(r) => r.why,
            other => panic!("{other:?}"),
        };
        assert!(why.contains("another user"), "{why}");
    }
    for mode in [0o777, 0o775] {
        fs::set_permissions(&h.home, fs::Permissions::from_mode(mode)).unwrap();
        let why = h.refused(&h.vars(None));
        assert!(why.contains("parent"), "{mode:o}: {why}");
    }
    fs::set_permissions(&h.home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(h.resolve(&h.vars(None)), RustupHome::Usable(_)));
}

#[test]
fn a_path_that_is_not_utf8_is_refused_with_its_reason() {
    let h = host();
    let parent = h.dir("x");
    let bad = parent
        .join(std::ffi::OsStr::from_bytes(b"r\xff"))
        .join(".rustup");
    fs::create_dir_all(bad.join("toolchains")).unwrap();
    let why = h.refused(&h.vars(Some(&bad)));
    assert!(why.contains("UTF-8"), "{why}");
}

#[test]
fn a_refusal_names_the_path_and_the_reason() {
    let r = Refused {
        path: PathBuf::from("/h/.rustup"),
        why: "it has no toolchains directory".into(),
    };
    assert_eq!(
        r.to_string(),
        "rustup home /h/.rustup was not used: it has no toolchains directory"
    );
}

/// The production argv leaves the toolchains visible and read-only. Needs a
/// working bwrap; it returns quietly where there is none.
#[test]
fn the_production_argv_reads_the_toolchains_but_cannot_write_them() {
    if sandbox_ready().is_err() || !Path::new(PYTHON).is_file() {
        eprintln!("bwrap unavailable: skipped");
        return;
    }
    let h = host();
    let rustup = h.rustup(".rustup");
    let r = rustup.to_str().unwrap();
    let checkout_dir = tempfile::tempdir().unwrap();
    let checkout = fs::canonicalize(checkout_dir.path()).unwrap();
    let scratch = checkout.join(".ap-service-t");
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.join(sub)).unwrap();
    }
    let proxy = Proxy::start(Arc::new(AllowedHosts::empty()), super::tests::test_asker()).unwrap();
    let hosts = proxy.write_hosts("hosts", &[]).unwrap();
    let binds = [rustup.clone()];
    let env = [("RUSTUP_HOME".to_string(), r.to_string())];
    let p = Profile {
        checkout: &checkout,
        scratch: &scratch,
        proxy_dir: proxy.dir(),
        extra_env: &env,
        ro_binds: &binds,
        hosts_file: &hosts,
        tunnels: &[],
    };
    let script = format!(
        "ls \"$RUSTUP_HOME/toolchains\" && ! mkdir \"$RUSTUP_HOME/toolchains/new\" \
         && ! touch \"$RUSTUP_HOME/x\" && ! rm -r \"$RUSTUP_HOME/toolchains\" 2>/dev/null; \
         test -d {r}/toolchains && echo visible; test -w {r} || echo read-only"
    );
    let args = bwrap_args(&p, &["/bin/sh".into(), "-c".into(), script]).unwrap();
    let out = std::process::Command::new(ap_core::sandbox::bwrap_path().unwrap())
        .env_clear()
        .args(&args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("stable-x86_64-unknown-linux-gnu"),
        "{stdout}"
    );
    assert!(
        stdout.contains("visible") && stdout.contains("read-only"),
        "{stdout}"
    );
    assert!(
        rustup
            .join("toolchains/stable-x86_64-unknown-linux-gnu")
            .is_dir()
    );
}
