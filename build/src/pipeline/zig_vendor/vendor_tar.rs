//! Writes a package's vendor archive: zig's global package cache with each
//! dependency in the layout its zig version reads, as a `vendor.tar.zst` with
//! `vendor/` as its only top-level directory. The output depends only on the
//! dependencies' files and links, their executable bits and their hashes: not
//! on file times, owners, the order they were given in, or the host.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
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
    /// Bytes built in memory: the packed `.tar.gz` of a dependency.
    Bytes(Vec<u8>),
}

type Entries = BTreeMap<String, Body>;

/// The `vendor.tar.zst` for `deps` in `layout`. Dependencies are written in
/// the order of their declared hashes. Two with one hash are an error.
pub(super) fn vendor_tar_zst(deps: &[Dep], layout: Layout) -> Result<Vec<u8>, String> {
    let mut sorted: Vec<&Dep> = deps.iter().collect();
    sorted.sort_by(|a, b| a.declared.cmp(&b.declared));
    if let Some(pair) = sorted.windows(2).find(|w| w[0].declared == w[1].declared) {
        return Err(format!("two dependencies declare {}", pair[0].declared));
    }
    let mut entries = Entries::new();
    put(&mut entries, "vendor".to_string(), Body::Dir)?;
    put(&mut entries, "vendor/p".to_string(), Body::Dir)?;
    for dep in sorted {
        let top = format!("vendor/p/{}", dep.declared);
        match layout {
            Layout::Packed => {
                let gz = packed_gz(&dep.declared, &dep.items)?;
                put(&mut entries, format!("{top}.tar.gz"), Body::Bytes(gz))?;
            }
            Layout::Unpacked => {
                for (path, body) in tree_entries(&top, &dep.items)? {
                    put(&mut entries, path, body)?;
                }
            }
        }
    }
    let mut tar = tar::Builder::new(
        zstd::stream::write::Encoder::new(Vec::new(), 9)
            .map_err(|e| format!("starting the zstd stream: {e}"))?,
    );
    for (path, body) in &entries {
        append(&mut tar, path, body)?;
    }
    let encoder = tar
        .into_inner()
        .map_err(|e| format!("finishing the tar: {e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("finishing the zstd stream: {e}"))
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

/// The `p/<hash>.tar.gz` of one package: `<hash>/` and its tree, gzipped.
fn packed_gz(declared: &str, items: &[Item]) -> Result<Vec<u8>, String> {
    let entries = tree_entries(declared, items)?;
    let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::best(),
    ));
    for (path, body) in &entries {
        append(&mut ar, path, body)?;
    }
    ar.into_inner()
        .map_err(|e| format!("finishing {declared}.tar.gz: {e}"))?
        .finish()
        .map_err(|e| format!("finishing {declared}.tar.gz: {e}"))
}

/// Append one entry with fixed metadata: mtime 0, owner and group 0 with no
/// names, mode 0755 for directories and executables, 0644 for other files.
fn append<W: Write>(ar: &mut tar::Builder<W>, path: &str, body: &Body) -> Result<(), String> {
    let err = |e: std::io::Error| format!("writing {path}: {e}");
    let mut h = tar::Header::new_gnu();
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_username("").map_err(err)?;
    h.set_groupname("").map_err(err)?;
    match body {
        Body::Dir => {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_mode(0o755);
            h.set_size(0);
            h.set_cksum();
            ar.append_data(&mut h, path, &[][..]).map_err(err)
        }
        Body::File { abs, exec } => {
            let mut file =
                std::fs::File::open(abs).map_err(|e| format!("reading {}: {e}", abs.display()))?;
            let len = file.metadata().map_err(err)?.len();
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(if *exec { 0o755 } else { 0o644 });
            h.set_size(len);
            h.set_cksum();
            ar.append_data(&mut h, path, &mut file).map_err(err)
        }
        Body::Link(target) => {
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_mode(0o777);
            h.set_size(0);
            ar.append_link(&mut h, path, Path::new(std::ffi::OsStr::from_bytes(target)))
                .map_err(err)
        }
        Body::Bytes(data) => {
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(0o644);
            h.set_size(data.len() as u64);
            h.set_cksum();
            ar.append_data(&mut h, path, data.as_slice()).map_err(err)
        }
    }
}
