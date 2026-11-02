//! Writes a package's vendor archive: zig's global package cache with each
//! dependency in the layout its zig version reads, as a `vendor.tar.zst` (or
//! `.tar.gz`) with the cache directory as its only top-level directory. The
//! output depends only on the dependencies' files and links, their executable
//! bits and their hashes: not on file times, owners, the order they were given
//! in, or the host.
//!
//! The archive is written as it is built: each file is copied from its place
//! in the tree, and each packed `.tar.gz` is first built in a scratch file,
//! since its size goes in the tar header before its bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::fetch::Dep;
use super::tree::Item;

/// The layout of a package in zig's global cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    /// `p/<hash>.tar.gz`, whose top directory is `<hash>/`. Zig 0.16 and later.
    Packed,
    /// `p/<hash>/` directories. Zig 0.15 and earlier.
    Unpacked,
}

impl Layout {
    /// The layout for a zig version such as `0.16.0` or `0.15.2`. Development
    /// builds (`0.16.0-dev.5+abc`) count by their major and minor numbers.
    pub(super) fn for_zig(version: &str) -> Result<Layout, String> {
        let nums: Vec<u32> = version
            .split(['.', '-', '+'])
            .take(2)
            .map(|p| p.parse::<u32>())
            .collect::<Result<_, _>>()
            .map_err(|_| format!("`{version}` is not a zig version"))?;
        match nums[..] {
            [major, minor] if (major, minor) >= (0, 16) => Ok(Layout::Packed),
            [_, _] => Ok(Layout::Unpacked),
            _ => Err(format!("`{version}` is not a zig version")),
        }
    }
}

/// What one entry of the archive holds.
enum Body {
    Dir,
    /// A file on disk, streamed into the archive.
    File {
        abs: PathBuf,
        exec: bool,
    },
    Link(Vec<u8>),
    /// The packed `.tar.gz` of the dependency at this index of the sorted
    /// list. It is built when its turn comes.
    Packed(usize),
}

type Entries = BTreeMap<String, Body>;

/// The compression of the vendor archive. It is the one the old archive had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Compression {
    Zst,
    Gz,
}

/// Write the vendor archive for `deps` in `layout` to `out`, with `root` (a
/// relative path such as `vendor`) as the directory zig's cache lives in.
/// Dependencies are written in the order of their declared hashes. Two with
/// one hash are an error. Each packed `.tar.gz` is built in a scratch file in
/// `scratch`.
pub(super) fn write_vendor_tar(
    deps: &[Dep],
    layout: Layout,
    root: &str,
    compression: Compression,
    scratch: &Path,
    out: impl Write,
) -> Result<(), String> {
    let mut sorted: Vec<&Dep> = deps.iter().collect();
    sorted.sort_by(|a, b| a.declared.cmp(&b.declared));
    if let Some(pair) = sorted.windows(2).find(|w| w[0].declared == w[1].declared) {
        return Err(format!("two dependencies declare {}", pair[0].declared));
    }
    let parts: Vec<&str> = root.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err(format!("`{root}` is not a plain relative directory"));
    }
    let mut entries = Entries::new();
    for depth in 1..=parts.len() {
        put(&mut entries, parts[..depth].join("/"), Body::Dir)?;
    }
    let p = format!("{root}/p");
    put(&mut entries, p.clone(), Body::Dir)?;
    for (i, dep) in sorted.iter().enumerate() {
        let top = format!("{p}/{}", dep.declared);
        match layout {
            Layout::Packed => put(&mut entries, format!("{top}.tar.gz"), Body::Packed(i))?,
            Layout::Unpacked => {
                for (path, body) in tree_entries(&top, &dep.items)? {
                    put(&mut entries, path, body)?;
                }
            }
        }
    }
    match compression {
        Compression::Zst => {
            let enc = zstd::stream::write::Encoder::new(out, 9)
                .map_err(|e| format!("starting the zstd stream: {e}"))?;
            let enc = write_tar(&entries, &sorted, scratch, enc)?;
            enc.finish()
                .map_err(|e| format!("finishing the zstd stream: {e}"))?;
        }
        Compression::Gz => {
            let enc = flate2::write::GzEncoder::new(out, flate2::Compression::best());
            let enc = write_tar(&entries, &sorted, scratch, enc)?;
            enc.finish()
                .map_err(|e| format!("finishing the gzip stream: {e}"))?;
        }
    }
    Ok(())
}

/// `entries` as a tar stream into `w`, which is handed back unfinished.
fn write_tar<W: Write>(
    entries: &Entries,
    sorted: &[&Dep],
    scratch: &Path,
    w: W,
) -> Result<W, String> {
    let mut tar = tar::Builder::new(w);
    for (path, body) in entries {
        match body {
            Body::Packed(i) => append_packed(&mut tar, path, sorted[*i], scratch)?,
            _ => append(&mut tar, path, body)?,
        }
    }
    tar.into_inner()
        .map_err(|e| format!("finishing the tar: {e}"))
}

/// Insert an entry. A directory may be listed twice; anything else may not.
fn put(entries: &mut Entries, path: String, body: Body) -> Result<(), String> {
    match entries.get(&path) {
        None => {
            entries.insert(path, body);
            Ok(())
        }
        Some(Body::Dir) if matches!(body, Body::Dir) => Ok(()),
        Some(_) => Err(format!("two entries for {path}")),
    }
}

/// The entries of one package's tree under `top`: the directory itself, every
/// directory above each item, and the items.
fn tree_entries(top: &str, items: &[Item]) -> Result<Entries, String> {
    let mut out = Entries::new();
    out.insert(top.to_string(), Body::Dir);
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for item in items {
        let rel = item.rel();
        let parts: Vec<&str> = rel.split('/').collect();
        for depth in 1..parts.len() {
            dirs.insert(format!("{top}/{}", parts[..depth].join("/")));
        }
        let path = format!("{top}/{rel}");
        let body = match item {
            Item::File { abs, exec, .. } => Body::File {
                abs: abs.clone(),
                exec: *exec,
            },
            Item::Link { target, .. } => Body::Link(target.clone()),
        };
        put(&mut out, path, body)?;
    }
    for dir in dirs {
        put(&mut out, dir, Body::Dir)?;
    }
    Ok(out)
}

/// Append the `p/<hash>.tar.gz` of one package: `<hash>/` and its tree, gzipped.
/// The gzipped tar goes to a scratch file first, so its size is known for the
/// header and no archive is held in memory.
fn append_packed<W: Write>(
    ar: &mut tar::Builder<W>,
    path: &str,
    dep: &Dep,
    scratch: &Path,
) -> Result<(), String> {
    let mut file = tempfile::tempfile_in(scratch)
        .map_err(|e| format!("creating a scratch file in {}: {e}", scratch.display()))?;
    let entries = tree_entries(&dep.declared, &dep.items)?;
    let mut inner = tar::Builder::new(flate2::write::GzEncoder::new(
        &mut file,
        flate2::Compression::best(),
    ));
    for (p, body) in &entries {
        append(&mut inner, p, body)?;
    }
    inner
        .into_inner()
        .map_err(|e| format!("finishing {}.tar.gz: {e}", dep.declared))?
        .finish()
        .map_err(|e| format!("finishing {}.tar.gz: {e}", dep.declared))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("reading {}.tar.gz back: {e}", dep.declared))?;
    let len = file
        .metadata()
        .map_err(|e| format!("reading {}.tar.gz back: {e}", dep.declared))?
        .len();
    let mut h = fixed_header(tar::EntryType::Regular, 0o644, len)
        .map_err(|e| format!("writing {path}: {e}"))?;
    h.set_cksum();
    ar.append_data(&mut h, path, &mut file)
        .map_err(|e| format!("writing {path}: {e}"))
}

/// A header with fixed metadata: mtime 0, owner and group 0 with no names.
fn fixed_header(kind: tar::EntryType, mode: u32, size: u64) -> std::io::Result<tar::Header> {
    let mut h = tar::Header::new_gnu();
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_username("")?;
    h.set_groupname("")?;
    h.set_entry_type(kind);
    h.set_mode(mode);
    h.set_size(size);
    Ok(h)
}

/// Append one entry with fixed metadata: mode 0755 for directories and
/// executables, 0644 for other files, and a link's own 0777.
fn append<W: Write>(ar: &mut tar::Builder<W>, path: &str, body: &Body) -> Result<(), String> {
    let err = |e: std::io::Error| format!("writing {path}: {e}");
    match body {
        Body::Dir => {
            let mut h = fixed_header(tar::EntryType::Directory, 0o755, 0).map_err(err)?;
            h.set_cksum();
            ar.append_data(&mut h, path, &[][..]).map_err(err)
        }
        Body::File { abs, exec } => {
            let mut file =
                std::fs::File::open(abs).map_err(|e| format!("reading {}: {e}", abs.display()))?;
            let len = file.metadata().map_err(err)?.len();
            let mode = if *exec { 0o755 } else { 0o644 };
            let mut h = fixed_header(tar::EntryType::Regular, mode, len).map_err(err)?;
            h.set_cksum();
            ar.append_data(&mut h, path, &mut file).map_err(err)
        }
        Body::Link(target) => {
            let mut h = fixed_header(tar::EntryType::Symlink, 0o777, 0).map_err(err)?;
            ar.append_link(&mut h, path, Path::new(std::ffi::OsStr::from_bytes(target)))
                .map_err(err)
        }
        Body::Packed(_) => unreachable!("a packed .tar.gz is appended by append_packed"),
    }
}
