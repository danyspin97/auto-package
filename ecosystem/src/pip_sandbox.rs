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
//!   directories (a venv, `/opt/python`). Of that prefix only its `bin`,
//!   `lib`, `lib64`, `include` and `share/python*` directories and its
//!   `pyvenv.cfg` are bound, never the rest of it (`~/tools` next to
//!   `~/tools/bin` keeps its own files out of the sandbox). A prefix with a
//!   credential-like file at its top level, such as `.netrc` or `id_ed25519`,
//!   is refused;
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
use std::path::{Component, Path, PathBuf};

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

/// The directories of a non-system interpreter prefix that are bound, at
/// their own path. `share/python*` is added by `prefix_mounts`.
const PREFIX_DIRS: &[&str] = &["bin", "lib", "lib64", "include"];

/// Names at the top level of an interpreter prefix that mark it as holding
/// credentials. Such a prefix is refused: a prefix is not bound whole, but its
/// interpreter directories are, and a credential beside them would be left
/// exposed to the build. Matched by `is_credential_entry`.
const CREDENTIAL_ENTRY_NAMES: &[&str] = &[
    ".netrc",
    ".pypirc",
    ".env",
    ".git-credentials",
    ".config",
    ".ssh",
    ".gnupg",
    ".aws",
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
            // The variable is passed on with only the entries that the
            // sandbox actually has; when none are usable it is not set.
            if let Some(usable) = bind_ca(&mut a, key, v, host)? {
                env.push((k.clone(), usable));
            }
            continue;
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
        let Some(prefix) = interpreter_prefix(file) else {
            continue;
        };
        check_prefix(program, &prefix, &host.home)?;
        prefixes.insert(prefix);
    }
    // Every prefix is outside the system directories (see `interpreter_prefix`),
    // so only its interpreter parts are bound, never the prefix itself: a
    // prefix such as `~/tools` holds whatever else the user keeps there.
    let mut bin_dirs = Vec::new();
    for prefix in &prefixes {
        for mount in prefix_mounts(prefix) {
            a.ro_bind(&mount, &mount);
        }
        let bin = prefix.join("bin");
        if bin.is_dir() {
            bin_dirs.push(bin);
        }
    }
    Ok((exe, bin_dirs))
}

/// The parts of a prefix outside the system directories that the sandbox gets,
/// each to be bound read-only at its own path: the interpreter's directories
/// (`bin`, `lib`, `lib64`, `include`, `share/python*`) and `pyvenv.cfg`, as
/// far as they exist. Nothing else at the top level of the prefix is bound.
fn prefix_mounts(prefix: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = PREFIX_DIRS
        .iter()
        .map(|d| prefix.join(d))
        .filter(|p| p.is_dir())
        .collect();
    if let Ok(entries) = std::fs::read_dir(prefix.join("share")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.as_bytes().starts_with(b"python") && entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    let cfg = prefix.join("pyvenv.cfg");
    if cfg.is_file() {
        out.push(cfg);
    }
    out.sort();
    out
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

/// The prefix to bind for `file`, or `None` when nothing needs binding.
///
/// A file under [`SYSTEM_DIRS`] is already in the sandbox through
/// `system_ro_binds`, whatever its prefix would be: `/bin/pip` has the prefix
/// `/`, which must never be bound (it holds the home directory), but `/bin` is
/// bound on its own. Any other file is bound by its prefix, which is checked.
fn interpreter_prefix(file: &Path) -> Option<PathBuf> {
    if is_under_any(file, SYSTEM_DIRS) {
        return None;
    }
    Some(prefix_of(file))
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

/// Whether `path` is at or under one of `SENSITIVE_HOME_DIRS` in the home
/// directory (`~/.local`, `~/.config`, `~/.cache`, at any depth).
fn under_sensitive_home_dir(path: &Path, home: &Path) -> bool {
    let real_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    [home, real_home.as_path()].into_iter().any(|h| {
        path.strip_prefix(h)
            .ok()
            .and_then(|rel| rel.components().next())
            .is_some_and(|first| SENSITIVE_HOME_DIRS.iter().any(|d| first.as_os_str() == *d))
    })
}

/// Whether `prefix` looks like a Python installation tree: an interpreter in
/// `bin/` and a standard library under `lib/python3*`, as a standalone build
/// (such as one managed by uv) has. An app's config directory with a `bin/`
/// directory but no Python library does not.
fn looks_like_interpreter_install(prefix: &Path) -> bool {
    let has_interpreter =
        prefix.join("bin/python3").is_file() || prefix.join("bin/python").is_file();
    let has_stdlib = std::fs::read_dir(prefix.join("lib"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().as_bytes().starts_with(b"python3") && e.path().is_dir());
    has_interpreter && has_stdlib
}

/// Whether `name`, an entry at the top level of a prefix, looks like a
/// credential: a netrc, a PyPI or git credential file, an SSH key, a PEM file,
/// a `credentials` file, or one of the credential directories.
fn is_credential_entry(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    CREDENTIAL_ENTRY_NAMES.contains(&name)
        || CREDENTIAL_HOME_DIRS.contains(&name)
        || name.starts_with(".env.")
        || name.starts_with("id_")
        || name.starts_with("credentials")
        || name.ends_with(".pem")
}

/// Whether the top level of `prefix` holds an entry that `is_credential_entry`
/// accepts. Only the top level is looked at.
fn has_credential_entry(prefix: &Path) -> bool {
    std::fs::read_dir(prefix)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| is_credential_entry(&e.file_name()))
}

fn check_prefix(program: &str, prefix: &Path, home: &Path) -> Result<(), SandboxError> {
    let reason = sensitive_reason(prefix, home)
        .or_else(|| {
            // Under the configuration or cache directories only a virtualenv
            // (which has a `pyvenv.cfg`) or an interpreter installation is a
            // prefix of its own; anything else there may be a tool's data, config
            // or keyring.
            (under_sensitive_home_dir(prefix, home)
                && !prefix.join("pyvenv.cfg").is_file()
                && !looks_like_interpreter_install(prefix))
            .then_some("it is inside a configuration or cache directory and is not a virtualenv or Python installation")
        })
        .or_else(|| {
            has_credential_entry(prefix)
                .then_some("it holds a credential file or directory at its top level")
        });
    match reason {
        None => Ok(()),
        Some(why) => Err(SandboxError::UnsafePrefix {
            program: program.to_string(),
            prefix: prefix.to_path_buf(),
            why,
        }),
    }
}

/// Why the file `path` must not be bound into the sandbox, checked as named
/// and as resolved through symlinks.
fn sensitive_file(path: &Path, home: &Path) -> Option<&'static str> {
    sensitive_reason(path, home).or_else(|| {
        let real = path.canonicalize().ok()?;
        sensitive_reason(&real, home)
    })
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
            if let Some(why) = sensitive_file(&src, &host.home) {
                tracing::warn!("skipping pip config {}: {why}", src.display());
                continue;
            }
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
    if let Some(why) = sensitive_file(&src, &host.home) {
        tracing::warn!("skipping PIP_CONFIG_FILE {}: {why}", src.display());
        return None;
    }
    a.ro_bind(&src, Path::new(SANDBOX_PIP_CONFIG_FILE));
    Some(SANDBOX_PIP_CONFIG_FILE.into())
}

/// Make the CA bundle files or directories named by `var` usable in the
/// sandbox. `SSL_CERT_DIR` is a colon-separated list and each entry is handled
/// as a single path; the others name one path.
///
/// An entry is bound at its own path when it exists and is not already in the
/// sandbox. An entry that is relative, missing or unusable is skipped with a
/// warning. An entry that names credentials is refused with an error, as
/// before.
///
/// Returns the value to pass into the sandbox: the usable entries joined the
/// same way, or `None` when no entry is usable and the variable should not be
/// set at all.
fn bind_ca(
    a: &mut Args,
    var: &str,
    value: &OsString,
    host: &Host,
) -> Result<Option<OsString>, SandboxError> {
    let raw = value.as_bytes();
    let sep = if var == "SSL_CERT_DIR" {
        Some(b':')
    } else {
        None
    };
    let entries: Vec<&[u8]> = match sep {
        Some(c) => raw.split(|b| *b == c).collect(),
        None => vec![raw],
    };
    let mut usable: Vec<Vec<u8>> = Vec::new();
    for entry in entries {
        if entry.is_empty() {
            continue;
        }
        let path = PathBuf::from(OsStr::from_bytes(entry));
        if !path.is_absolute() {
            tracing::warn!(
                "{var} entry {} is not an absolute path; not exposing it to the sandbox",
                path.display()
            );
            continue;
        }
        if !path.exists() {
            tracing::warn!(
                "{var} entry {} does not exist; not exposing it to the sandbox",
                path.display()
            );
            continue;
        }
        // `..` is resolved before anything is decided: `/usr/../home/u/.ssh/key`
        // is not under `/usr`. A path that climbs above `/` is not exposed.
        let Some(normal) = normalize_lexically(&path) else {
            tracing::warn!(
                "{var} entry {} climbs above the root; not exposing it to the sandbox",
                path.display()
            );
            continue;
        };
        let Ok(resolved) = path.canonicalize() else {
            tracing::warn!(
                "{var} entry {} cannot be resolved; not exposing it to the sandbox",
                path.display()
            );
            continue;
        };
        // Both the path as named and what it resolves to through symlinks and
        // `..` must be free of credentials. `sensitive_file` also resolves the
        // normalized path, which is what gets bound.
        if let Some(why) =
            sensitive_reason(&resolved, &host.home).or_else(|| sensitive_file(&normal, &host.home))
        {
            return Err(SandboxError::UnsafePath {
                var: var.to_string(),
                path,
                why,
            });
        }
        if is_under_any(&normal, SYSTEM_DIRS) || is_under_any(&normal, ETC_ENTRIES) {
            // Already present in the sandbox at this path.
            usable.push(normal.as_os_str().as_bytes().to_vec());
            continue;
        }
        a.ro_bind(&normal, &normal);
        usable.push(normal.as_os_str().as_bytes().to_vec());
    }
    if usable.is_empty() {
        return Ok(None);
    }
    let joined: Vec<u8> = usable.join(&b':');
    Ok(Some(OsStr::from_bytes(&joined).to_owned()))
}

/// `path` with its `.` and `..` components resolved without touching the
/// filesystem: `/usr/../home/u` is `/home/u`. `None` when `path` is not
/// absolute or a `..` would climb above `/`.
fn normalize_lexically(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => out.push(name),
            Component::Prefix(_) => return None,
        }
    }
    out.is_absolute().then_some(out)
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
    fn ca_dir_entries_are_bound_one_by_one_and_only_usable_ones_are_passed_on() {
        let mut f = fixture();
        let good = f.home.join("corp-certs");
        fs::create_dir_all(&good).unwrap();
        let good_s = good.to_string_lossy().into_owned();
        let value = format!("/nonexistent/one:{good_s}::/nonexistent/two");
        f.host.vars.push(("SSL_CERT_DIR".into(), value.into()));
        let args = build(&f);
        let b = binds(&args);
        assert!(b.contains(&("--ro-bind".into(), good_s.clone(), good_s.clone())));
        assert!(!b.iter().any(|(_, s, _)| s.starts_with("/nonexistent")));
        // Only the entry that is bound survives in the sandbox's environment.
        let env = setenvs(&args);
        assert!(env.contains(&("SSL_CERT_DIR".into(), good_s)), "{env:?}");
    }

    #[test]
    fn a_missing_ca_file_is_not_passed_on_to_the_sandbox() {
        let mut f = fixture();
        f.host
            .vars
            .push(("SSL_CERT_FILE".into(), "/nonexistent/ca.pem".into()));
        let args = build(&f);
        assert!(!setenvs(&args).iter().any(|(k, _)| k == "SSL_CERT_FILE"));
        assert!(
            !binds(&args)
                .iter()
                .any(|(_, s, _)| s.contains("nonexistent"))
        );
    }

    /// `/usr/..` is `/`, so the key is not under `/usr`: the check must see
    /// the normalized path, not the prefix it was written with.
    #[test]
    fn a_ca_path_climbing_out_of_usr_to_credentials_is_refused() {
        let mut f = fixture();
        let key = f.home.join(".ssh/id_ed25519");
        let through = format!("/usr/..{}", key.display());
        f.host.vars.push(("SSL_CERT_FILE".into(), through.into()));
        let err = bwrap_args(&["pip"], &f.host).unwrap_err();
        assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
    }

    #[test]
    fn a_ca_path_under_etc_ssl_is_still_accepted() {
        let path = "/etc/ssl/certs/ca-bundle.crt";
        if !Path::new(path).exists() {
            eprintln!("{path} is absent on this host; nothing to check");
            return;
        }
        let mut f = fixture();
        f.host.vars.push(("SSL_CERT_FILE".into(), path.into()));
        let args = build(&f);
        assert!(setenvs(&args).contains(&("SSL_CERT_FILE".into(), path.into())));
        assert!(!binds(&args).iter().any(|(_, s, _)| s.contains(".ssh")));
    }

    #[test]
    fn normalize_lexically_resolves_dot_dot_and_refuses_climbing_above_root() {
        assert_eq!(
            normalize_lexically(Path::new("/usr/../home/x/./y")),
            Some(PathBuf::from("/home/x/y"))
        );
        assert_eq!(normalize_lexically(Path::new("/usr/../../x")), None);
        assert_eq!(
            normalize_lexically(Path::new("/")),
            Some(PathBuf::from("/"))
        );
        assert_eq!(normalize_lexically(Path::new("rel/x")), None);
    }

    #[test]
    fn a_ca_dir_entry_naming_credentials_is_still_refused() {
        let mut f = fixture();
        let key = f.home.join(".ssh");
        let value = format!("{}:{}", f.home.join("corp-certs").display(), key.display());
        f.host.vars.push(("SSL_CERT_DIR".into(), value.into()));
        let err = bwrap_args(&["pip"], &f.host).unwrap_err();
        assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
    }

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
}
