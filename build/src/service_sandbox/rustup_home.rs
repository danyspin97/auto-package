//! The host's rustup home, bound read-only into the sandbox of a service that
//! runs cargo. `/usr/bin/cargo` can be rustup's proxy; without the toolchains
//! it finds none and refuses to run. Only that one directory is exposed:
//! never `$HOME`, a parent of it or `~/.cargo` (registry tokens live there).
//! The directory is picked by an allow-list: it must be named `.rustup` or be
//! the very path the user set in `RUSTUP_HOME`, and hold a `toolchains`
//! directory.

use super::*;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;

/// The variables the rustup home is derived from.
#[derive(Debug, Default, Clone)]
pub(crate) struct HostVars {
    pub rustup_home: Option<OsString>,
    pub home: Option<PathBuf>,
    pub cargo_home: Option<OsString>,
}

impl HostVars {
    pub(crate) fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        Self {
            rustup_home: var("RUSTUP_HOME"),
            home: var("HOME").map(PathBuf::from),
            cargo_home: var("CARGO_HOME"),
        }
    }
}

/// A rustup home that exists but is not bound, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub path: PathBuf,
    pub why: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rustup home {} was not used: {}",
            self.path.display(),
            self.why
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RustupHome {
    /// Nothing is set up: no candidate path, or it does not exist.
    Absent,
    /// The canonical directory to bind read-only.
    Usable(PathBuf),
    Refused(Refused),
}

/// The real directory behind `candidate`, or why it must not be bound.
pub(crate) fn resolve_rustup_home(vars: &HostVars, uid: u32) -> RustupHome {
    let explicit = vars.rustup_home.as_ref().map(PathBuf::from);
    let candidate = match (&explicit, &vars.home) {
        (Some(p), _) => p.clone(),
        (None, Some(h)) => h.join(".rustup"),
        (None, None) => return RustupHome::Absent,
    };
    match fs::symlink_metadata(&candidate) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return RustupHome::Absent,
        Err(e) => {
            return RustupHome::Refused(Refused {
                path: candidate,
                why: format!("cannot be read: {e}"),
            });
        }
    }
    match check_rustup_home(&candidate, explicit.is_some(), vars, uid) {
        Ok(real) => RustupHome::Usable(real),
        Err(why) => RustupHome::Refused(Refused {
            path: candidate,
            why,
        }),
    }
}

fn owned_by(meta: &fs::Metadata, uid: u32) -> bool {
    meta.uid() == uid || meta.uid() == 0
}

fn check_rustup_home(
    candidate: &Path,
    explicit: bool,
    vars: &HostVars,
    uid: u32,
) -> Result<PathBuf, String> {
    let io = |what: &str, e: io::Error| format!("{what}: {e}");
    let home = vars.home.as_deref().ok_or("HOME is not set")?;
    if !home.is_absolute() || !candidate.is_absolute() {
        return Err("the path is not absolute".to_string());
    }
    let link = fs::symlink_metadata(candidate).map_err(|e| io("cannot be read", e))?;
    let real = fs::canonicalize(candidate).map_err(|e| io("cannot be resolved", e))?;
    if link.file_type().is_symlink() {
        let beside = candidate
            .parent()
            .and_then(|p| fs::canonicalize(p).ok())
            .ok_or("its parent cannot be resolved")?;
        if real.parent() != Some(beside.as_path()) {
            return Err("it is a link that leaves its directory".to_string());
        }
    }
    if real.to_str().is_none() {
        return Err("the path is not valid UTF-8".to_string());
    }
    let named = real.file_name().is_some_and(|n| n == ".rustup");
    if !(named || explicit && real == candidate) {
        return Err(
            "it is not named .rustup and RUSTUP_HOME does not name it directly".to_string(),
        );
    }
    let meta = fs::metadata(&real).map_err(|e| io("cannot be read", e))?;
    if !meta.is_dir() {
        return Err("it is not a directory".to_string());
    }
    if !owned_by(&meta, uid) {
        return Err("it is owned by another user".to_string());
    }
    if meta.mode() & 0o022 != 0 {
        return Err("it is writable by its group or others".to_string());
    }
    let toolchains = fs::symlink_metadata(real.join("toolchains")).ok();
    if !toolchains.is_some_and(|m| m.is_dir()) {
        return Err("it has no toolchains directory".to_string());
    }
    if real.parent().is_none() {
        return Err("it is the root directory".to_string());
    }
    let home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if home.starts_with(&real) {
        return Err("it is the home directory or a parent of it".to_string());
    }
    let cargo_home = vars
        .cargo_home
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cargo"));
    let cargo_home = fs::canonicalize(&cargo_home).unwrap_or(cargo_home);
    if real.starts_with(&cargo_home)
        || cargo_home.starts_with(&real)
        || real.components().any(|c| c.as_os_str() == ".cargo")
    {
        return Err("it is, contains or is inside the cargo home".to_string());
    }
    // Everything above it must be the user's or root's, and nobody else may
    // replace an entry (a sticky directory like /tmp protects ours).
    for dir in real.ancestors().skip(1) {
        let m = fs::metadata(dir).map_err(|e| io("a parent cannot be read", e))?;
        let sticky = m.mode() & 0o1000 != 0;
        if !owned_by(&m, uid) || (m.mode() & 0o022 != 0 && !sticky) {
            return Err(format!(
                "its parent {} is owned by another user or writable by its group or others",
                dir.display()
            ));
        }
    }
    Ok(real)
}

/// The host's rustup home to bind for the invoking user.
pub(crate) fn host_rustup_home() -> RustupHome {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    resolve_rustup_home(&HostVars::from_env(), uid)
}
