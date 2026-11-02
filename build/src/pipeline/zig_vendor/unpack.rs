//! Unpacks a dependency archive into a directory, and refuses what the
//! vendoring step must not write: absolute or climbing paths, entries that
//! pass through a link, hard links, devices and fifos, and anything over the
//! limits.
//!
//! Symbolic links are never created on disk. Their targets are kept as they
//! are, as zig keeps them for hashing, and returned to the caller, which hashes
//! and archives them from this list. So a link that points outside the tree,
//! or at an absolute path, is recorded and does no harm here.
//!
//! The root is chosen as zig chooses it: when every entry is under one name
//! and that name is a directory, the root is that directory, and its name is
//! dropped. Otherwise the archive's top level is the root, so a file at the top
//! level or a second top-level directory is kept as it is. An archive whose
//! entries are all under `./` is refused, as zig 0.15.2 cannot unpack it.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

mod guard;

use super::scratch::Scratch;

pub(super) mod zip_archive;

/// The formats a dependency archive may have: the ones zig 0.15.2 reads. A
/// `.tar.bz2` or a `.gz` alone is refused, as zig refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Tar,
    TarGz,
    TarXz,
    TarZst,
    Zip,
}

impl Format {
    /// The format a URL's extension names, ignoring case, a query and a
    /// fragment. The extensions are the ones zig reads.
    pub(super) fn of_extension(url: &str) -> Option<Format> {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Format::TarGz)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Format::TarXz)
        } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
            Some(Format::TarZst)
        } else if name.ends_with(".tar") {
            Some(Format::Tar)
        } else if name.ends_with(".zip") {
            Some(Format::Zip)
        } else {
            None
        }
    }

    /// The format a response's `Content-Type` names, as zig reads it. A type
    /// with parameters is read without them, and its case is ignored.
    /// `application/octet-stream` names none, so the URL decides. Any other
    /// type is an error, as it is for zig.
    pub(super) fn of_content_type(content_type: &str) -> Result<Option<Format>, String> {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        Ok(match essence.as_str() {
            "application/octet-stream" => None,
            "application/x-tar" => Some(Format::Tar),
            "application/gzip"
            | "application/x-gzip"
            | "application/tar+gzip"
            | "application/x-tar-gz"
            | "application/x-gtar-compressed" => Some(Format::TarGz),
            "application/x-xz" => Some(Format::TarXz),
            "application/zstd" => Some(Format::TarZst),
            "application/zip" | "application/x-zip-compressed" => Some(Format::Zip),
            _ => {
                return Err(format!(
                    "unrecognized Content-Type `{content_type}` (zig_vendor reads tar, gzip, xz, zstd and zip archives)"
                ));
            }
        })
    }

    /// The format of a fetched archive, as zig 0.15.2 reads it. A response with
    /// no Content-Type is refused, as zig refuses it, whatever its URL says. Else
    /// the Content-Type decides when it names a format, and the URL's extension
    /// (`by_url`) decides when it is `application/octet-stream`.
    pub(super) fn resolve(
        content_type: Option<&str>,
        by_url: Option<Format>,
    ) -> Result<Format, String> {
        let Some(ct) = content_type else {
            return Err(
                "the response has no Content-Type, which zig 0.15.2 refuses too".to_string(),
            );
        };
        Format::of_content_type(ct)?.or(by_url).ok_or_else(|| {
            "the archive is not a format zig_vendor reads: its URL has no .tar.gz, .tar.xz, .tar.zst, .tar or .zip extension, and its Content-Type names none".to_string()
        })
    }
}

/// A symbolic link of an unpacked archive: its path relative to the root, and
/// its target exactly as the archive gave it.
pub(super) type Link = (PathBuf, PathBuf);

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

/// The limits for a dependency: 1 GiB of unpacked files and 100,000 entries,
/// set high so that real dependencies pass. A refusal names the limit.
pub(super) const LIMITS: Limits = Limits {
    max_bytes: 1 << 30,
    max_entries: 100_000,
    max_depth: 32,
};

/// Bytes a tar header and its padding may take per entry, beyond the content.
const PER_ENTRY_OVERHEAD: u64 = 1536;

/// Unpack `src` into `dest`, which must not exist yet. Returns the links of the
/// archive, relative to the root that was chosen.
///
/// The entries are written into a staging directory first, since whether the
/// archive has a single root directory is known only at its end.
pub(super) fn unpack(
    src: impl Read,
    format: Format,
    dest: &Path,
    limits: Limits,
) -> Result<Vec<Link>, String> {
    let stage_path = stage_of(dest);
    if dest.exists() || stage_path.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    // Removed when this returns, on failure too; after a success it is empty.
    let stage = Scratch::create(stage_path)?;
    let stage_dir = stage.path();
    let (links, roots) = if format == Format::Zip {
        zip_archive::write_entries(src, stage_dir, limits)?;
        (Vec::new(), stage_roots(stage_dir)?)
    } else {
        write_entries(src, format, stage_dir, limits)?
    };
    let top = single_top_dir(stage_dir, &roots)?;
    let links = match &top {
        Some(name) => links
            .into_iter()
            .map(|(rel, target)| {
                let inner = rel.strip_prefix(name).unwrap_or(&rel).to_path_buf();
                (inner, target)
            })
            .collect(),
        None => links,
    };
    match top {
        Some(name) => std::fs::rename(stage_dir.join(&name), dest)
            .map_err(|e| format!("moving the root into {}: {e}", dest.display()))?,
        None => std::fs::rename(stage_dir, dest)
            .map_err(|e| format!("moving {} into place: {e}", dest.display()))?,
    }
    Ok(links)
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
/// top-level names. Links are not written: each is returned, as `(link,
/// target)`, and an entry may not pass through one. Every entry's root name is
/// returned too, for [`single_top_dir`].
fn write_entries(
    src: impl Read,
    format: Format,
    dest: &Path,
    limits: Limits,
) -> Result<(Vec<Link>, BTreeSet<OsString>), String> {
    let stream: Box<dyn Read> = match format {
        Format::Tar => Box::new(src),
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
    let mut links: Vec<Link> = Vec::new();
    let mut link_set: HashSet<PathBuf> = HashSet::new();
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
            refuse_through_link(&link_set, &rel)?;
            ensure_dirs(dest, &rel)?;
        } else if kind.is_file() {
            refuse_through_link(&link_set, &rel)?;
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
            if target.as_os_str().is_empty() {
                return Err(format!("{} has no link target", rel.display()));
            }
            refuse_through_link(&link_set, parent_of(&rel))?;
            if link_set.contains(&rel) || std::fs::symlink_metadata(dest.join(&rel)).is_ok() {
                return Err(format!(
                    "{} is written twice: as a link and as a file or directory",
                    rel.display()
                ));
            }
            ensure_dirs(dest, parent_of(&rel))?;
            link_set.insert(rel.clone());
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

/// Refuse an entry whose path is a link, or passes through one, in the archive.
/// Links are not on disk, so `ensure_dirs` cannot see them.
fn refuse_through_link(links: &HashSet<PathBuf>, rel: &Path) -> Result<(), String> {
    let mut at = PathBuf::new();
    for c in rel.components() {
        at.push(c);
        if links.contains(&at) {
            return Err(format!(
                "{} is or passes through a link in the archive, which zig_vendor refuses",
                rel.display()
            ));
        }
    }
    Ok(())
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
