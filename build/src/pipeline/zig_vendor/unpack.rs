//! Unpacks a dependency archive into a directory, and refuses what the
//! vendoring step must not write: absolute or climbing paths, links that leave
//! the tree or pass through another link, hard links, devices and fifos, and
//! anything over the limits.
//!
//! The root is chosen as zig chooses it: when every entry is under one name
//! and that name is a directory, the root is that directory, and its name is
//! dropped. Otherwise the archive's top level is the root, so a file at the top
//! level or a second top-level directory is kept as it is. An archive whose
//! entries are all under `./` is refused, as zig 0.15.2 cannot unpack it.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

mod guard;

pub(super) mod zip_archive;

/// The compressions a dependency archive may use. `.tar.bz2` and the rest are
/// refused, as zig refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    TarGz,
    TarXz,
    TarZst,
    Zip,
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
            Ok(Format::Zip)
        } else {
            Err(format!(
                "`{url}` is not a .tar.gz, .tar.xz, .tar.zst or .zip archive (the only formats zig_vendor reads)"
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
///
/// The entries are written into a staging directory first, since whether the
/// archive has a single root directory is known only at its end. Links are
/// checked again then, against the root that was chosen.
pub(super) fn unpack(
    src: impl Read,
    format: Format,
    dest: &Path,
    limits: Limits,
) -> Result<(), String> {
    let stage = stage_of(dest);
    if dest.exists() || stage.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    std::fs::create_dir_all(&stage).map_err(|e| format!("creating {}: {e}", stage.display()))?;
    let (links, roots) = if format == Format::Zip {
        zip_archive::write_entries(src, &stage, limits)?;
        (Vec::new(), stage_roots(&stage)?)
    } else {
        write_entries(src, format, &stage, limits)?
    };
    let top = single_top_dir(&stage, &roots)?;
    check_links(&links, top.is_some())?;
    match top {
        Some(name) => {
            std::fs::rename(stage.join(&name), dest)
                .map_err(|e| format!("moving the root into {}: {e}", dest.display()))?;
            std::fs::remove_dir(&stage).map_err(|e| format!("removing {}: {e}", stage.display()))
        }
        None => std::fs::rename(&stage, dest)
            .map_err(|e| format!("moving {} into place: {e}", dest.display())),
    }
}

/// The most bytes an archive's stream may hold, decompressed: the content
/// limit plus a header allowance per entry. The reader stops there.
fn stream_cap(limits: Limits) -> u64 {
    limits
        .max_bytes
        .saturating_add((limits.max_entries as u64).saturating_mul(PER_ENTRY_OVERHEAD))
}

/// The staging directory for `dest`: a sibling with a `.stage` suffix.
fn stage_of(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".stage");
    PathBuf::from(name)
}

/// The names on disk at the top of `stage`: the roots of a zip archive, whose
/// entries are unpacked by `zip_archive` and so have no names of their own here.
fn stage_roots(stage: &Path) -> Result<BTreeSet<OsString>, String> {
    let read = |e: std::io::Error| format!("reading {}: {e}", stage.display());
    let mut roots = BTreeSet::new();
    for entry in std::fs::read_dir(stage).map_err(read)? {
        roots.insert(entry.map_err(read)?.file_name());
    }
    Ok(roots)
}

/// The root zig chooses: when every entry is under one name, `roots` holds
/// that name alone, and it is stripped when it is a directory. `roots` is
/// what the entries name as written, so `./x` is under `.`, and zig does
/// not strip `.` (it cannot unpack such an archive at all).
fn single_top_dir(stage: &Path, roots: &BTreeSet<OsString>) -> Result<Option<OsString>, String> {
    let mut names = roots.iter();
    let (Some(name), None) = (names.next(), names.next()) else {
        return Ok(None);
    };
    if name == "." {
        return Err(
            "every entry is under ./, which zig 0.15.2 cannot unpack: name the entries without ./"
                .to_string(),
        );
    }
    match std::fs::symlink_metadata(stage.join(name)) {
        Ok(meta) => Ok(meta.is_dir().then(|| name.clone())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", stage.display())),
    }
}

/// The first component of an entry's name as written: `.` for a name that
/// starts with `./`, which is how zig names an entry under the archive's root.
fn raw_root(path: &Path) -> Option<OsString> {
    match path.components().next()? {
        Component::Normal(name) => Some(name.to_owned()),
        Component::CurDir => Some(OsString::from(".")),
        _ => None,
    }
}

/// Write every entry of the archive under `dest`, with the archive's own
/// top-level names. Returns each link written, as `(link, target)`, and the
/// root name of every entry, as [`single_top_dir`] reads them.
fn write_entries(
    src: impl Read,
    format: Format,
    dest: &Path,
    limits: Limits,
) -> Result<(Vec<(PathBuf, PathBuf)>, BTreeSet<OsString>), String> {
    let stream: Box<dyn Read> = match format {
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(src)),
        // Zig reads one xz stream and stops at its end, so a second stream
        // makes the archive truncated, as it does for zig.
        Format::TarXz => Box::new(lzma_rust2::XzReader::new(src, false)),
        Format::TarZst => Box::new(
            zstd::stream::read::Decoder::new(src)
                .map_err(|e| format!("reading the zstd stream: {e}"))?,
        ),
        Format::Zip => unreachable!("unpack reads zip archives with zip_archive"),
    };
    let mut ar = tar::Archive::new(guard::MetaGuard::new(stream).take(stream_cap(limits)));
    let mut links = Vec::new();
    let mut roots = BTreeSet::new();
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
        let rel = entry_path(&path)?;
        if let Some(root) = raw_root(&path) {
            roots.insert(root);
        }
        if rel.as_os_str().is_empty() {
            if kind.is_dir() {
                continue;
            }
            return Err(format!("the entry {} has no name", path.display()));
        }
        // The top-level name counts as one level, as in `max_depth`'s doc.
        if rel.components().count() > limits.max_depth + 1 {
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
            links.push((rel, target));
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
    Ok((links, roots))
}

/// An entry's path as normal components. A leading `.` is skipped, as zig
/// skips it; the root, a second `.` after a name, and `..` are refused.
fn entry_path(path: &Path) -> Result<PathBuf, String> {
    let shown = path.display();
    let mut rel = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => rel.push(p),
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("the entry {shown} is not a relative path"));
            }
            Component::CurDir => {}
            _ => return Err(format!("the entry {shown} has an unsafe component")),
        }
    }
    Ok(rel)
}

/// Refuse a link that leaves the root. With a single top-level directory the
/// root is that directory, so `..` may not climb above it.
fn check_links(links: &[(PathBuf, PathBuf)], under_top: bool) -> Result<(), String> {
    let floor = usize::from(under_top);
    for (rel, target) in links {
        if leaves(rel, target, floor) {
            return Err(format!(
                "the link {} points outside the tree",
                rel.display()
            ));
        }
    }
    Ok(())
}

/// Whether the link `target` at `rel` climbs above its first `floor`
/// components. The check is lexical, from the link's own directory.
fn leaves(rel: &Path, target: &Path, floor: usize) -> bool {
    if target.as_os_str().is_empty() || target.has_root() {
        return true;
    }
    let mut depth = parent_of(rel).components().count();
    for c in target.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth <= floor {
                    return true;
                }
                depth -= 1;
            }
            _ => return true,
        }
    }
    false
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
