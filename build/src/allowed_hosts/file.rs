//! The user's `allowed-hosts` file, in `$XDG_CONFIG_HOME/auto-package/`
//! (default `~/.config/auto-package/`). One host per line; `#` starts a
//! comment, and on a host line the text after `#` is the reason:
//!
//! ```text
//! # Written by `auto-package hosts allow`; edit freely.
//! mirror.example.org  # our package mirror
//! ```
//!
//! A line that is not a host, a comment or blank is reported with its line
//! number and ignored. The file is created with mode 0600 in a 0700
//! directory and replaced atomically, so a reader sees the old or the new
//! content, never a part.

use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::{Entry, Origin, normalize_host, normalize_user_host, printable};

pub const DIR_NAME: &str = "auto-package";
pub const FILE_NAME: &str = "allowed-hosts";
/// The lock held across each read-modify-write of the file.
pub const LOCK_NAME: &str = "allowed-hosts.lock";

/// The directory holding the file, from `XDG_CONFIG_HOME` (when absolute) or
/// `HOME`. `None` when neither gives an absolute path.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`config_dir`] for the given variables, so it can be tested.
pub fn config_dir_in(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })?;
    Some(base.join(DIR_NAME))
}

/// The file itself, when there is a config directory.
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

/// What one line of the file says.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    Blank,
    /// A host (as written, not yet checked) and its reason.
    Host {
        raw: String,
        reason: Option<String>,
    },
    Malformed(String),
}

fn split_line(line: &str) -> Line {
    // Control characters (an ANSI escape, a tab) are not shown as they are.
    let line = printable(line);
    let (before, reason) = match line.split_once('#') {
        Some((b, r)) => (b, Some(r.trim().to_string()).filter(|r| !r.is_empty())),
        None => (line.as_str(), None),
    };
    let before = before.trim();
    if before.is_empty() {
        return Line::Blank;
    }
    if before.contains(char::is_whitespace) {
        return Line::Malformed(format!(
            "`{before}` is more than one host; put one host per line"
        ));
    }
    Line::Host {
        raw: before.to_string(),
        reason,
    }
}

/// The entries of a file's text, and what could not be read from it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// `line N: why`, one per refused line.
    pub problems: Vec<String>,
}

/// Parse the text of the file. A host repeated keeps its first line.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        match split_line(line) {
            Line::Blank => {}
            Line::Malformed(why) => out.problems.push(format!("line {n}: {why}")),
            Line::Host { raw, reason } => match normalize_user_host(&raw) {
                Ok(host) => {
                    if !out.entries.iter().any(|e| e.host == host) {
                        out.entries.push(Entry {
                            host,
                            origin: Origin::File,
                            reason,
                        });
                    }
                }
                Err(e) => out.problems.push(format!("line {n}: {e}")),
            },
        }
    }
    out
}

/// Read the file at `path`. A missing file is an empty list, not a problem.
pub fn load(path: &Path) -> Parsed {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == ErrorKind::NotFound => Parsed::default(),
        Err(e) => Parsed {
            entries: Vec::new(),
            problems: vec![format!("cannot read {}: {e}", path.display())],
        },
    }
}

/// What [`add_host`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    New,
    AlreadyPresent,
}

/// Append `host` (with an optional reason) to the file at `path`, creating the
/// file and its directory with private modes. Other lines are kept as they are.
pub fn add_host(path: &Path, host: &str, reason: Option<&str>) -> Result<Added, String> {
    let host = normalize_user_host(host)?;
    let _lock = lock_for(path)?;
    let target = resolve_target(path);
    let text = read_or_empty(&target)?;
    if parse(&text).entries.iter().any(|e| e.host == host) {
        return Ok(Added::AlreadyPresent);
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&host);
    if let Some(reason) = reason.map(printable).map(|r| r.trim().to_string())
        && !reason.is_empty()
    {
        out.push_str(&format!("  # {reason}"));
    }
    out.push('\n');
    write_atomic(&target, &out)?;
    Ok(Added::New)
}

/// Remove the lines that name `host` from the file at `path`. Returns whether
/// one was there. Comments and other lines are kept.
pub fn remove_host(path: &Path, host: &str) -> Result<bool, String> {
    let host = normalize_host(host)?;
    let _lock = lock_for(path)?;
    let target = resolve_target(path);
    let text = read_or_empty(&target)?;
    let mut removed = false;
    let mut out = String::new();
    for line in text.lines() {
        let same = matches!(
            split_line(line),
            Line::Host { ref raw, .. } if normalize_host(raw).as_deref() == Ok(host.as_str())
        );
        if same {
            removed = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if removed {
        write_atomic(&target, &out)?;
    }
    Ok(removed)
}

/// Where the list is really kept: `path`, or the file a symlink at `path`
/// points to, with the directory of that file resolved. A symlink is never
/// replaced, so the write goes through it to its target. Only the path is
/// resolved: no directory is created or changed here, so the target's
/// directory keeps its own mode and holds no lock file.
pub fn resolve_target(path: &Path) -> PathBuf {
    let mut target = path.to_path_buf();
    for _ in 0..8 {
        let is_link = fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            break;
        }
        let Ok(link) = fs::read_link(&target) else {
            break;
        };
        target = if link.is_absolute() {
            link
        } else {
            target.parent().map_or(link.clone(), |d| d.join(&link))
        };
    }
    let dir = target.parent().map(Path::to_path_buf);
    match (dir, target.file_name().map(|n| n.to_os_string())) {
        (Some(dir), Some(name)) => fs::canonicalize(&dir).unwrap_or(dir).join(name),
        _ => target,
    }
}

/// The exclusive lock of the config directory (the directory of `path` as
/// the user named it, where a symlink sits, not where it points), held across
/// a read and the write that follows, so two `hosts` commands cannot lose each
/// other's line.
fn lock_for(path: &Path) -> Result<fs::File, String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    ensure_private_dir(dir)?;
    let lock_path = dir.join(LOCK_NAME);
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("cannot lock {}: {e}", lock_path.display()))?;
    Ok(lock)
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Replace the file at `path` (a resolved target, see [`resolve_target`]) with
/// `text`: a temporary file in the same directory, synced, then renamed over
/// the old one. An existing file keeps its mode; a new one is 0600. The
/// directory is not changed.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    let err = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", path.display());
    let mode = fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| err("write", e))?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode))
        .map_err(|e| err("write", e))?;
    tmp.write_all(text.as_bytes())
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| err("write", e))?;
    tmp.persist(path).map_err(|e| err("write", e.error))?;
    Ok(())
}

/// `dir` exists and is private to the user (0700). Parents are created with
/// the usual modes; only the last component is private.
fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))
}
