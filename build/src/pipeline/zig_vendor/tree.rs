//! The entries of an unpacked dependency that its `.paths` filter keeps, and the
//! zig hash over them. Links are read, never followed.

use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use super::hash::PackageHash;

/// One entry the package keeps, relative to its root with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Item {
    File {
        rel: String,
        abs: PathBuf,
        /// Whether the file had an executable bit. Not hashed; the vendor
        /// archive keeps it.
        exec: bool,
    },
    Link {
        rel: String,
        target: Vec<u8>,
    },
}

impl Item {
    pub(super) fn rel(&self) -> &str {
        match self {
            Item::File { rel, .. } | Item::Link { rel, .. } => rel,
        }
    }
}

/// Check each `.paths` entry: relative, without `..`. `None` (no field) keeps
/// everything.
fn check_paths(paths: &[String]) -> Result<(), String> {
    for p in paths {
        let bad = p.starts_with('/')
            || Path::new(p)
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::RootDir));
        if bad {
            return Err(format!(".paths entry `{p}` is absolute or climbs out"));
        }
    }
    Ok(())
}

/// Whether `paths` keeps the file or link `rel`. Zig's rule: an entry names a
/// file or a directory (its contents), `""` and `"."` name everything, and a
/// trailing `/` or a leading `./` is ignored.
pub(super) fn path_kept(paths: Option<&[String]>, rel: &str) -> bool {
    let Some(paths) = paths else {
        return true;
    };
    paths.iter().any(|p| {
        let mut p = p.as_str();
        while let Some(rest) = p.strip_prefix("./") {
            p = rest;
        }
        let p = p.trim_end_matches('/');
        if p.is_empty() || p == "." {
            return true;
        }
        rel == p || rel.strip_prefix(p).is_some_and(|r| r.starts_with('/'))
    })
}

/// Every regular file under `root`, and every link of `links` (the links the
/// unpacker kept in memory), that `paths` keeps, sorted by path. Directories
/// are walked, not listed. A link on disk, or a special file, is an error: the
/// unpacker writes neither.
pub(super) fn included_items(
    root: &Path,
    links: &[(PathBuf, PathBuf)],
    paths: Option<&[String]>,
) -> Result<Vec<Item>, String> {
    if let Some(paths) = paths {
        check_paths(paths)?;
    }
    let mut out = Vec::new();
    walk(root, root, paths, &mut out)?;
    for (link, target) in links {
        let rel = rel_string(link)?;
        if path_kept(paths, &rel) {
            out.push(Item::Link {
                rel,
                target: target.as_os_str().as_bytes().to_vec(),
            });
        }
    }
    out.sort_by(|a, b| a.rel().as_bytes().cmp(b.rel().as_bytes()));
    Ok(out)
}

fn walk(
    root: &Path,
    dir: &Path,
    paths: Option<&[String]>,
    out: &mut Vec<Item>,
) -> Result<(), String> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("reading {}: {e}", dir.display()))?;
    names.sort();
    for name in names {
        let abs = dir.join(&name);
        let rel = rel_of(root, &abs)?;
        let meta = std::fs::symlink_metadata(&abs)
            .map_err(|e| format!("reading {}: {e}", abs.display()))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            walk(root, &abs, paths, out)?;
            continue;
        }
        if kind.is_symlink() {
            return Err(format!(
                "{rel} is a link on disk, which unpacking never writes"
            ));
        }
        if !path_kept(paths, &rel) {
            continue;
        }
        if kind.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let exec = meta.permissions().mode() & 0o111 != 0;
            out.push(Item::File { rel, abs, exec });
        } else {
            return Err(format!("{rel} is not a file, a directory or a link"));
        }
    }
    Ok(())
}

/// `abs` relative to `root`, with `/` separators.
fn rel_of(root: &Path, abs: &Path) -> Result<String, String> {
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| "a path left the tree".to_string())?;
    rel_string(rel)
}

/// A relative path as a string with `/` separators.
fn rel_string(rel: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(p) => parts.push(
                p.to_str()
                    .ok_or_else(|| format!("{} is not UTF-8", rel.display()))?,
            ),
            _ => return Err(format!("{} is not a plain relative path", rel.display())),
        }
    }
    Ok(parts.join("/"))
}

/// The per-file digests of `items`, as zig hashes a package.
fn collect(items: &[Item]) -> Result<PackageHash, String> {
    let mut hash = PackageHash::new();
    for item in items {
        match item {
            Item::File { rel, abs, .. } => {
                let mut file = std::fs::File::open(abs)
                    .map_err(|e| format!("reading {}: {e}", abs.display()))?;
                hash.file(rel, &mut file)
                    .map_err(|e| format!("reading {}: {e}", abs.display()))?;
            }
            Item::Link { rel, target } => hash.link(rel, target),
        }
    }
    Ok(hash)
}

/// The zig package hash of `items` with the fingerprint id `id`, the base64
/// after the name and version in `build.zig.zon`.
pub(super) fn package_hash(items: &[Item], id: u32) -> Result<String, String> {
    collect(items)?.finish(id)
}

/// The legacy `1220…` hash of `items`, as zig 0.13 and earlier declared it.
pub(super) fn legacy_hash(items: &[Item]) -> Result<String, String> {
    Ok(collect(items)?.finish_legacy())
}
