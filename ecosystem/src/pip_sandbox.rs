//! The bubblewrap sandbox pip runs in.
//!
//! Resolving a package can build its sdist, and building runs the sdist's
//! `setup.py` or PEP 517 backend (and installs its `build-system.requires`),
//! all of it code from the index running as the user. The sandbox gives that
//! code:
//!
//! - the system read-only (`/usr`, `/lib*`, `/bin`, `/sbin`, and only the
//!   `/etc` entries pip, TLS, DNS and compilers need),
//! - an empty `tmpfs` home (`/home/sandbox`), `/tmp` and `/run`, which are the
//!   only writable places and vanish with the process; pip's cache lives
//!   there too, so nothing a backend writes survives to poison a later run,
//! - the network (resolution needs the index),
//! - its own PID, IPC, UTS, cgroup and user namespaces, a new session (no
//!   terminal injection) and `--die-with-parent`.
//!
//! It does not get the real home directory, `~/.ssh`, `~/.config`, XDG
//! directories, `/run/user` (D-Bus, ssh-agent and gpg-agent sockets), `/var`,
//! `/opt` or any environment variable beyond the pip allowlist.
//!
//! Three things are bound in on purpose, each read-only and each as a single
//! file or directory, never a parent:
//!
//! - the interpreter prefix, when pip or its Python live outside the system
//!   directories (a venv, `/opt/python`);
//! - pip's own config (`PIP_CONFIG_FILE`, `~/.config/pip/pip.conf`,
//!   `~/.pip/pip.conf`), since private indexes are configured there. Index
//!   URLs in those files, and in `PIP_INDEX_URL` and `PIP_EXTRA_INDEX_URL`,
//!   can carry credentials, and a backend can read them. They are passed on
//!   as they are, because stripping them would break private indexes; use an
//!   index credential that is only good for reading packages;
//! - `~/.netrc`, only when `AUTO_PACKAGE_PIP_NETRC=1`, for the same reason.
//!   The default is off because a netrc usually holds credentials for hosts
//!   other than the index.
//!
//! Custom CA bundles named by `SSL_CERT_FILE`, `SSL_CERT_DIR`,
//! `REQUESTS_CA_BUNDLE` and `PIP_CERT` are bound at the same path when they
//! live outside the system directories.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The home directory inside the sandbox.
pub const SANDBOX_HOME: &str = "/home/sandbox";

/// Where the user's `PIP_CONFIG_FILE` appears inside the sandbox.
const SANDBOX_PIP_CONFIG_FILE: &str = "/home/sandbox/.pip-config-file";

/// Set to `1` to give the sandbox `~/.netrc`. Off by default.
pub const NETRC_ENV: &str = "AUTO_PACKAGE_PIP_NETRC";

use ap_core::sandbox::ETC_ENTRIES;

/// Directories the sandbox already has from the system mounts.
const SYSTEM_DIRS: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64"];

/// Directories directly under the home directory that are never bound whole:
/// the prefix of a tool installed at `~/.local/bin/pip` is `~/.local`, which
/// also holds keyrings. Venvs deeper inside (`~/.local/share/pipx/venvs/x`)
/// are fine.
const SENSITIVE_HOME_DIRS: &[&str] = &[".local", ".config", ".cache"];

/// Credential directories nothing may be bound from, at any depth.
const CREDENTIAL_HOME_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".docker",
    ".password-store",
    ".mozilla",
    ".thunderbird",
];

/// Variables set explicitly inside the sandbox, so the host's values are not
/// copied.
const OWN_VARS: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "PIP_CONFIG_FILE",
];

/// Variables holding a path to a CA bundle file.
const CA_FILE_VARS: &[&str] = &["SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "PIP_CERT"];

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The program is not on `PATH`; callers treat this like a failed launch.
    #[error("{0} not found on PATH")]
    ProgramNotFound(String),

    #[error(
        "cannot sandbox {program}: it lives under {prefix}, which holds more than the \
         interpreter ({why}). Install pip in a virtualenv or use the system pip, or pass \
         --unsafe-resolve to resolve without the sandbox"
    )]
    UnsafePrefix {
        program: String,
        prefix: PathBuf,
        why: &'static str,
    },

    #[error("{var} points at {path}, which is {why}; not exposing it to the sandbox")]
    UnsafePath {
        var: String,
        path: PathBuf,
        why: &'static str,
    },
}

/// What the host contributes to one sandboxed pip run.
pub struct Host {
    /// The real home directory. Nothing in it is bound except the files named
    /// in the module documentation.
    pub home: PathBuf,
    /// The host's `PATH`, used to find the program.
    pub path: OsString,
    /// The allowlisted variables of the host (see `pip::pip_env`).
    pub vars: Vec<(OsString, OsString)>,
    /// Whether `~/.netrc` is shared with the sandbox.
    pub netrc: bool,
}

/// The `bwrap` arguments for running `program` (a pip command line prefix),
/// up to and including the program itself. The caller appends pip's own
/// arguments.
pub fn bwrap_args(program: &[&str], host: &Host) -> Result<Vec<OsString>, SandboxError> {
    let mut a = Args::default();
    a.flags([
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--cap-drop",
        "ALL",
    ]);
    a.flags(ap_core::sandbox::system_ro_binds());
    a.flags(ap_core::sandbox::etc_ro_binds());
    a.flags(["--proc", "/proc", "--dev", "/dev"]);
    a.flags(["--tmpfs", "/tmp", "--tmpfs", "/run"]);
    a.flags(["--tmpfs", SANDBOX_HOME]);
    a.flags(["--dir", "/home/sandbox/.cache"]);

    let (exe, bin_dirs) = bind_interpreter(&mut a, program[0], host)?;
    bind_pip_config(&mut a, host);

    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for (k, v) in &host.vars {
        let Some(key) = k.to_str() else { continue };
        if OWN_VARS.contains(&key) {
            continue;
        }
        if CA_FILE_VARS.contains(&key) || key == "SSL_CERT_DIR" {
            bind_ca(&mut a, key, v, host)?;
        }
        env.push((k.clone(), v.clone()));
    }
    if let Some(cfg) = pip_config_env(&mut a, host) {
        env.push(("PIP_CONFIG_FILE".into(), cfg));
    }
    // Nothing but the tmpfs mounts above is writable: the root itself is a
    // tmpfs that would otherwise accept files (lost on exit, but writable).
    a.flags(["--remount-ro", "/"]);
    let mut path = OsString::new();
    for dir in bin_dirs {
        path.push(dir);
        path.push(":");
    }
    path.push("/usr/local/bin:/usr/bin:/bin");
    env.push(("PATH".into(), path));
    env.push(("HOME".into(), SANDBOX_HOME.into()));
    env.push(("TMPDIR".into(), "/tmp".into()));
    env.push(("XDG_CACHE_HOME".into(), "/home/sandbox/.cache".into()));
    env.push(("XDG_CONFIG_HOME".into(), "/home/sandbox/.config".into()));
    for (k, v) in env {
        a.0.push("--setenv".into());
        a.0.push(k);
        a.0.push(v);
    }

    a.flags(["--chdir", "/tmp", "--"]);
    a.0.push(exe.into_os_string());
    a.0.extend(program[1..].iter().map(OsString::from));
    Ok(a.0)
}

#[derive(Default)]
struct Args(Vec<OsString>);

impl Args {
    fn flags<S: AsRef<OsStr>>(&mut self, flags: impl IntoIterator<Item = S>) {
        self.0
            .extend(flags.into_iter().map(|f| f.as_ref().to_owned()));
    }

    fn ro_bind(&mut self, src: &Path, dst: &Path) {
        self.0.push("--ro-bind".into());
        self.0.push(src.into());
        self.0.push(dst.into());
    }
}

/// Locate `program` and bind its interpreter prefix. Returns the executable
/// to run and the `bin` directories to put on the sandbox `PATH`.
fn bind_interpreter(
    a: &mut Args,
    program: &str,
    host: &Host,
) -> Result<(PathBuf, Vec<PathBuf>), SandboxError> {
    let exe = which(program, &host.path)
        .ok_or_else(|| SandboxError::ProgramNotFound(program.to_string()))?;
    // The program as found, what it resolves to, and the interpreter named by
    // its shebang (as found and resolved): a venv's `python` is a symlink
    // into the system, and both ends matter.
    let mut files = vec![exe.clone()];
    if let Ok(real) = exe.canonicalize() {
        files.push(real);
    }
    if let Some(interp) = shebang_interpreter(&exe, &host.path) {
        if let Ok(real) = interp.canonicalize() {
            files.push(real);
        }
        files.push(interp);
    }

    let mut prefixes: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &files {
        let prefix = prefix_of(file);
        if is_under_any(&prefix, SYSTEM_DIRS) {
            continue;
        }
        check_prefix(program, &prefix, &host.home)?;
        prefixes.insert(prefix);
    }
    // A prefix inside another needs no mount of its own.
    let top: Vec<PathBuf> = prefixes
        .iter()
        .filter(|p| !prefixes.iter().any(|q| q != *p && p.starts_with(q)))
        .cloned()
        .collect();
    let mut bin_dirs = Vec::new();
    for prefix in &top {
        a.ro_bind(prefix, prefix);
        let bin = prefix.join("bin");
        if bin.is_dir() {
            bin_dirs.push(bin);
        }
    }
    Ok((exe, bin_dirs))
}

/// Find `program` the way a shell would: as given when it has a slash,
/// otherwise in each absolute `path` entry.
fn which(program: &str, path: &OsStr) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return (p.is_absolute() && p.is_file()).then_some(p);
    }
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// The interpreter named by `file`'s `#!` line, when it is a script.
fn shebang_interpreter(file: &Path, path: &OsStr) -> Option<PathBuf> {
    let mut head = [0u8; 256];
    let n = std::fs::File::open(file).ok()?.read(&mut head).ok()?;
    let rest = head[..n].strip_prefix(b"#!")?;
    let line = rest.split(|b| *b == b'\n').next()?;
    let mut words = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| OsStr::from_bytes(w).to_owned());
    let first = PathBuf::from(words.next()?);
    if first.file_name() == Some(OsStr::new("env")) {
        let name = words.find(|w| !w.as_bytes().starts_with(b"-"))?;
        return which(name.to_str()?, path);
    }
    first.is_absolute().then_some(first)
}

/// The directory an installed program belongs to: the parent of its `bin`
/// directory (a venv or install prefix), or its own directory otherwise.
fn prefix_of(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("/"));
    if dir.file_name() == Some(OsStr::new("bin")) {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

fn is_under_any(path: &Path, dirs: &[&str]) -> bool {
    dirs.iter().any(|d| path.starts_with(d))
}

/// Why `path`, a directory the sandbox would get, is too broad or too
/// sensitive to bind, if it is. `home` is the real home directory.
fn sensitive_reason(path: &Path, home: &Path) -> Option<&'static str> {
    let real_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    for h in [home, real_home.as_path()] {
        if h.starts_with(path) {
            return Some("it contains the home directory");
        }
        let Ok(rel) = path.strip_prefix(h) else {
            continue;
        };
        let mut parts = rel.components();
        let Some(first) = parts.next() else { continue };
        let first = first.as_os_str();
        if CREDENTIAL_HOME_DIRS.iter().any(|d| first == *d) {
            return Some("it is inside a credential directory");
        }
        if SENSITIVE_HOME_DIRS.iter().any(|d| first == *d) && parts.next().is_none() {
            return Some("it is a credential or configuration directory");
        }
    }
    None
}

fn check_prefix(program: &str, prefix: &Path, home: &Path) -> Result<(), SandboxError> {
    match sensitive_reason(prefix, home) {
        None => Ok(()),
        Some(why) => Err(SandboxError::UnsafePrefix {
            program: program.to_string(),
            prefix: prefix.to_path_buf(),
            why,
        }),
    }
}

/// Bind the user's pip config files, each at the place pip looks for it in
/// the sandbox home.
fn bind_pip_config(a: &mut Args, host: &Host) {
    let xdg = host
        .vars
        .iter()
        .find(|(k, _)| k == "XDG_CONFIG_HOME")
        .map(|(_, v)| PathBuf::from(v))
        .filter(|p| p.is_absolute());
    let mut candidates = vec![
        (
            host.home.join(".config/pip/pip.conf"),
            "/home/sandbox/.config/pip/pip.conf",
        ),
        (
            host.home.join(".pip/pip.conf"),
            "/home/sandbox/.pip/pip.conf",
        ),
    ];
    if let Some(xdg) = xdg {
        // pip reads `$XDG_CONFIG_HOME/pip/pip.conf` as well; in the sandbox
        // `XDG_CONFIG_HOME` is `~/.config`, so it lands in the same place.
        candidates.insert(
            0,
            (
                xdg.join("pip/pip.conf"),
                "/home/sandbox/.config/pip/pip.conf",
            ),
        );
    }
    let mut seen_dst = BTreeSet::new();
    for (src, dst) in candidates {
        if src.is_file() && seen_dst.insert(dst) {
            a.ro_bind(&src, Path::new(dst));
        }
    }
    if host.netrc {
        let netrc = host.home.join(".netrc");
        if netrc.is_file() {
            a.ro_bind(&netrc, Path::new("/home/sandbox/.netrc"));
        }
    }
}

/// The sandbox's `PIP_CONFIG_FILE`, binding the host's file when it names one.
fn pip_config_env(a: &mut Args, host: &Host) -> Option<OsString> {
    let (_, value) = host.vars.iter().find(|(k, _)| k == "PIP_CONFIG_FILE")?;
    // `os.devnull` is how pip is told to read no config at all.
    if value == "/dev/null" {
        return Some(value.clone());
    }
    let src = PathBuf::from(value);
    if !src.is_absolute() || !src.is_file() {
        return None;
    }
    a.ro_bind(&src, Path::new(SANDBOX_PIP_CONFIG_FILE));
    Some(SANDBOX_PIP_CONFIG_FILE.into())
}

/// Bind a CA bundle (file or directory) named by `var` at its own path when
/// the sandbox does not already have it.
fn bind_ca(a: &mut Args, var: &str, value: &OsString, host: &Host) -> Result<(), SandboxError> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || !path.exists() {
        return Ok(());
    }
    if is_under_any(&path, SYSTEM_DIRS) || is_under_any(&path, ETC_ENTRIES) {
        return Ok(());
    }
    let reason = sensitive_reason(&path, &host.home).or_else(|| {
        let real = path.canonicalize().ok()?;
        sensitive_reason(&real, &host.home)
    });
    if let Some(why) = reason {
        return Err(SandboxError::UnsafePath {
            var: var.to_string(),
            path,
            why,
        });
    }
    a.ro_bind(&path, &path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
                dst == venv || !dst.starts_with(&home),
                "{dst} is under the real home: {args:?}"
            );
            if src.starts_with(&home) {
                assert_eq!(src, venv, "unexpected host path bound: {src}");
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
                || src == venv;
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
        assert!(
            setenvs(&args).contains(&("PIP_CONFIG_FILE".into(), SANDBOX_PIP_CONFIG_FILE.into()))
        );

        f.host.vars.retain(|(k, _)| k != "PIP_CONFIG_FILE");
        f.host
            .vars
            .push(("PIP_CONFIG_FILE".into(), "/dev/null".into()));
        let args = build(&f);
        assert!(setenvs(&args).contains(&("PIP_CONFIG_FILE".into(), "/dev/null".into())));
    }

    #[test]
    fn a_ca_bundle_outside_the_system_is_bound_at_its_own_path() {
        let mut f = fixture();
        let ca = f.home.join("corp-ca.pem");
        fs::write(&ca, "pem").unwrap();
        f.host
            .vars
            .push(("SSL_CERT_FILE".into(), ca.clone().into()));
        f.host
            .vars
            .push(("REQUESTS_CA_BUNDLE".into(), "/etc/ssl/ca-bundle.pem".into()));
        let b = binds(&build(&f));
        let ca = ca.to_string_lossy().into_owned();
        assert!(b.contains(&("--ro-bind".into(), ca.clone(), ca)));
        assert!(!b.iter().any(|(_, s, _)| s == "/etc/ssl/ca-bundle.pem"));
    }

    #[test]
    fn a_ca_variable_pointing_at_credentials_is_refused() {
        let mut f = fixture();
        let key = f.home.join(".ssh/id_ed25519");
        f.host.vars.push(("SSL_CERT_FILE".into(), key.into()));
        let err = bwrap_args(&["pip"], &f.host).unwrap_err();
        assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
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
        assert!(binds(&args).contains(&("--ro-bind".into(), venv.clone(), venv)));
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
}
