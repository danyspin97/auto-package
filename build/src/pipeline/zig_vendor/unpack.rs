//! Unpacks a dependency archive into a directory, and refuses what the
//! vendoring step must not write: absolute or climbing paths, links that leave
//! the tree or pass through another link, hard links, devices and fifos, a
//! second top-level directory, and anything over the limits.
//!
//! The archive's single top-level directory is stripped, so the files land
//! directly in `dest`, as zig stores a package.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

mod guard;

/// The compressions a dependency archive may use. `.zip`, `.tar.bz2` and the
/// rest are refused, so their decoders are not parsed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    TarGz,
    TarXz,
    TarZst,
}

impl Format {
    /// The format named by a URL's extension (the URL has no query or fragment).
    pub(super) fn of_url(url: &str) -> Result<Format, String> {
        let path = url.rsplit('/').next().unwrap_or(url);
        if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            Ok(Format::TarGz)
        } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
            Ok(Format::TarXz)
        } else if path.ends_with(".tar.zst") || path.ends_with(".tzst") {
            Ok(Format::TarZst)
        } else if path.ends_with(".zip") {
            Err(format!(
                "`{url}` is a zip archive, which zig_vendor refuses"
            ))
        } else {
            Err(format!(
                "`{url}` is not a .tar.gz, .tar.xz or .tar.zst archive (the only formats zig_vendor reads)"
            ))
        }
    }
}

/// What an unpack may write. Checked while the archive is read, so a bomb
/// stops at the limit, not after it has filled the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Limits {
    /// Total bytes of regular files.
    pub(super) max_bytes: u64,
    /// Entries in the archive, directories and links included.
    pub(super) max_entries: usize,
    /// Components below the top-level directory.
    pub(super) max_depth: usize,
}

/// The limits for a real dependency. A large dependency is a few megabytes.
pub(super) const LIMITS: Limits = Limits {
    max_bytes: 512 << 20,
    max_entries: 50_000,
    max_depth: 32,
};

/// Bytes a tar header and its padding may take per entry, beyond the content.
const PER_ENTRY_OVERHEAD: u64 = 1536;

/// Unpack `src` into `dest`, which must not exist yet.
pub(super) fn unpack(
    src: impl Read,
    format: Format,
    dest: &Path,
    limits: Limits,
) -> Result<(), String> {
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("creating {}: {e}", dest.display()))?;
    let stream: Box<dyn Read> = match format {
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(src)),
        // Zig reads one xz stream and stops at its end, so a second stream
        // makes the archive truncated, as it does for zig.
        Format::TarXz => Box::new(lzma_rust2::XzReader::new(src, false)),
        Format::TarZst => Box::new(
            zstd::stream::read::Decoder::new(src)
                .map_err(|e| format!("reading the zstd stream: {e}"))?,
        ),
    };
    // The tar stream cannot be larger than the content plus headers.
    let stream_cap = limits
        .max_bytes
        .saturating_add((limits.max_entries as u64).saturating_mul(PER_ENTRY_OVERHEAD));
    let mut ar = tar::Archive::new(guard::MetaGuard::new(stream).take(stream_cap));
    let mut top: Option<std::ffi::OsString> = None;
    let mut bytes: u64 = 0;
    let entries = ar
        .entries()
        .map_err(|e| format!("reading the archive: {e}"))?;
    for (n, entry) in entries.enumerate() {
        if n >= limits.max_entries {
            return Err(format!("more than {} entries", limits.max_entries));
        }
        let mut entry = entry.map_err(|e| format!("reading the archive: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("an entry name is not readable: {e}"))?
            .into_owned();
        let kind = entry.header().entry_type();
        // A PAX global header describes the archive, not an entry. Zig skips
        // it too; it still counts above, as an entry of the archive.
        if kind.is_pax_global_extensions() {
            continue;
        }
        let Some(rel) = strip_top(&path, &mut top)? else {
            if kind.is_dir() {
                continue;
            }
            return Err(format!(
                "the top-level entry {} is not a directory",
                path.display()
            ));
        };
        if rel.components().count() > limits.max_depth {
            return Err(format!(
                "{} is deeper than {} levels",
                rel.display(),
                limits.max_depth
            ));
        }
        if kind.is_dir() {
            ensure_dirs(dest, &rel)?;
        } else if kind.is_file() {
            let size = entry.size();
            bytes = bytes.saturating_add(size);
            if bytes > limits.max_bytes {
                return Err(format!("more than {} bytes of files", limits.max_bytes));
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            write_file(dest, &rel, &mut entry, mode)?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|e| format!("a link target is not readable: {e}"))?
                .ok_or_else(|| format!("{} has no link target", rel.display()))?
                .into_owned();
            check_link(dest, &rel, &target)?;
            ensure_dirs(dest, parent_of(&rel))?;
            symlink(&target, dest.join(&rel))
                .map_err(|e| format!("creating the link {}: {e}", rel.display()))?;
        } else if kind.is_hard_link() {
            return Err(format!(
                "{} is a hard link, which zig_vendor refuses",
                rel.display()
            ));
        } else {
            return Err(format!(
                "{} is a device, a fifo or another special entry, which zig_vendor refuses",
                rel.display()
            ));
        }
    }
    if top.is_none() {
        return Err("the archive is empty".to_string());
    }
    Ok(())
}

/// The path below the archive's top-level directory, or `None` for the
/// top-level directory itself. Every entry must share one top-level name.
fn strip_top(path: &Path, top: &mut Option<std::ffi::OsString>) -> Result<Option<PathBuf>, String> {
    let shown = path.display();
    let mut parts = path.components();
    let first = match parts.next() {
        Some(Component::Normal(p)) => p,
        _ => return Err(format!("the entry {shown} is not a relative path")),
    };
    match top {
        None => *top = Some(first.to_owned()),
        Some(t) if t.as_os_str() != first => {
            return Err("the archive has more than one top-level directory".to_string());
        }
        Some(_) => {}
    }
    let mut rel = PathBuf::new();
    for c in parts {
        match c {
            Component::Normal(p) => rel.push(p),
            _ => return Err(format!("the entry {shown} has an unsafe component")),
        }
    }
    Ok(if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel)
    })
}

fn parent_of(rel: &Path) -> &Path {
    rel.parent().unwrap_or(Path::new(""))
}

/// Create the directories of `rel` under `dest`. A component that is already
/// a link or a file is refused: a later entry must not write through a link.
fn ensure_dirs(dest: &Path, rel: &Path) -> Result<(), String> {
    let mut at = dest.to_path_buf();
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return Err(format!("{} is not a plain relative path", rel.display()));
        };
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} passes through a link or a file in the archive",
                    rel.display()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&at).map_err(|e| format!("creating {}: {e}", at.display()))?;
            }
            Err(e) => return Err(format!("reading {}: {e}", at.display())),
        }
    }
    Ok(())
}

/// Create one regular file, executable if its mode had an executable bit.
fn write_file(dest: &Path, rel: &Path, input: &mut impl Read, mode: u32) -> Result<(), String> {
    ensure_dirs(dest, parent_of(rel))?;
    let at = dest.join(rel);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&at)
        .map_err(|e| format!("creating {}: {e}", rel.display()))?;
    std::io::copy(input, &mut out).map_err(|e| format!("writing {}: {e}", rel.display()))?;
    out.flush()
        .map_err(|e| format!("writing {}: {e}", rel.display()))?;
    let perm = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(perm))
        .map_err(|e| format!("setting the mode of {}: {e}", rel.display()))
}

/// Refuse a link whose target leaves the tree, or passes through a link that
/// is already on disk. The check is lexical from the link's directory, so
/// `..` pops only real directories of the tree.
fn check_link(dest: &Path, rel: &Path, target: &Path) -> Result<(), String> {
    let escape = || format!("the link {} points outside the tree", rel.display());
    if target.as_os_str().is_empty() || target.has_root() {
        return Err(escape());
    }
    let mut dirs: Vec<&std::ffi::OsStr> = Vec::new();
    for c in parent_of(rel).components() {
        match c {
            Component::Normal(p) => dirs.push(p),
            _ => return Err(escape()),
        }
    }
    let parts: Vec<Component<'_>> = target.components().collect();
    for (i, c) in parts.iter().enumerate() {
        match c {
            Component::Normal(p) => {
                let last = i + 1 == parts.len();
                if !last && on_disk_link(dest, &dirs, p) {
                    return Err(escape());
                }
                dirs.push(p);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if dirs.pop().is_none() {
                    return Err(escape());
                }
            }
            _ => return Err(escape()),
        }
    }
    Ok(())
}

/// Whether `name` under the directories `dirs` is a link on disk.
fn on_disk_link(dest: &Path, dirs: &[&std::ffi::OsStr], name: &std::ffi::OsStr) -> bool {
    let mut at = dest.to_path_buf();
    for d in dirs {
        at.push(d);
    }
    at.push(name);
    std::fs::symlink_metadata(at).is_ok_and(|m| m.file_type().is_symlink())
}
