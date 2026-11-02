//! The version-control metadata of a checkout (`.osc`, `.git`) as a turn leaves
//! it. osc reads `.osc/_apiurl` for the API server and its credentials, and git
//! runs the programs its config names, so a rewritten file redirects the
//! pipeline's next call. The Edit and Write denies and the quilt rules stop the
//! direct and the spelled-out forms; this check holds against the rest (a glob
//! the shell expands, a patch that creates a file). The metadata is copied when
//! a turn starts, compared when it ends, and put back when it changed.
//!
//! The copy is taken per turn, not once per run: osc calls the pipeline makes
//! between turns change the metadata, and they are the pipeline's to make.

use super::tools::CHECKOUT_METADATA_DIRS;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// Largest file whose bytes are kept, so a turn's change to it can be undone.
/// A larger one is compared by length and digest: a change is reported and the
/// turn rejected, but the bytes cannot be put back.
const KEEP_CAP: u64 = 16 * 1024 * 1024;

/// How many changed paths a rejection names before it says "and N more".
const NAMED_CHANGES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Dir,
    /// `digest` is `None` only for a git pack over [`KEEP_CAP`], compared by length.
    File {
        len: u64,
        digest: Option<[u8; 32]>,
    },
    Link(PathBuf),
    /// A fifo, socket or device: nothing to keep, so it cannot be put back.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    /// Permission bits; zero for a link.
    mode: u32,
    kind: Kind,
}

/// A node as it was copied: its bytes too, when they were kept.
struct Saved {
    node: Node,
    bytes: Option<Vec<u8>>,
}

/// The metadata tree, keyed by path relative to the checkout.
type Tree = BTreeMap<PathBuf, Saved>;

/// A file under `.git/objects`. Only a large one (a pack) is compared by length
/// alone; see [`file_kind`].
fn is_object_file(rel: &Path) -> bool {
    rel.starts_with(Path::new(".git").join("objects"))
}

/// The metadata of `checkout`, walked without following links. With `keep`,
/// the bytes of the files under [`KEEP_CAP`] are kept for a restore.
fn scan(checkout: &Path, keep: bool) -> Tree {
    let mut tree = Tree::new();
    for dir in CHECKOUT_METADATA_DIRS {
        scan_into(&checkout.join(dir), Path::new(dir), keep, &mut tree);
    }
    tree
}

fn scan_into(path: &Path, rel: &Path, keep: bool, tree: &mut Tree) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let ft = meta.file_type();
    let (node, bytes) = if ft.is_symlink() {
        let kind = match std::fs::read_link(path) {
            Ok(target) => Kind::Link(target),
            Err(_) => Kind::Other,
        };
        (Node { mode: 0, kind }, None)
    } else if ft.is_dir() {
        let mode = meta.permissions().mode() & 0o7777;
        (
            Node {
                mode,
                kind: Kind::Dir,
            },
            None,
        )
    } else if ft.is_file() {
        let mode = meta.permissions().mode() & 0o7777;
        let (kind, bytes) = file_kind(path, rel, meta.len(), keep);
        (Node { mode, kind }, bytes)
    } else {
        (
            Node {
                mode: 0,
                kind: Kind::Other,
            },
            None,
        )
    };
    let is_dir = ft.is_dir();
    tree.insert(rel.to_path_buf(), Saved { node, bytes });
    if is_dir && let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            scan_into(&entry.path(), &rel.join(entry.file_name()), keep, tree);
        }
    }
}

/// The kind of a regular file, with its bytes when they are kept. A file that
/// cannot be read is recorded without a digest.
fn file_kind(path: &Path, rel: &Path, len: u64, keep: bool) -> (Kind, Option<Vec<u8>>) {
    // A large git object is a pack: hashing it on every turn is not worth it,
    // and it is compared by length. Smaller objects are compared like any file.
    if len > KEEP_CAP && is_object_file(rel) {
        return (Kind::File { len, digest: None }, None);
    }
    if keep && len <= KEEP_CAP {
        return match std::fs::read(path) {
            Ok(bytes) => (
                Kind::File {
                    len: bytes.len() as u64,
                    digest: Some(Sha256::digest(&bytes).into()),
                },
                Some(bytes),
            ),
            Err(_) => (Kind::File { len, digest: None }, None),
        };
    }
    let digest = stream_digest(path);
    (Kind::File { len, digest }, None)
}

/// The SHA-256 of a file read in chunks, `None` when it cannot be read.
fn stream_digest(path: &Path) -> Option<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().into())
}

/// What a turn changed, as `(path, "added" | "removed" | "modified")`, sorted.
fn changes(before: &Tree, after: &Tree) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    for (path, now) in after {
        match before.get(path) {
            None => out.push((path.clone(), "added")),
            Some(old) if old.node != now.node => out.push((path.clone(), "modified")),
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            out.push((path.clone(), "removed"));
        }
    }
    out.sort();
    out
}

/// Whether `now` has the same kind of node as `old` (a file, a directory or a
/// link), so that a mode or a byte change can be undone in place.
fn same_kind(old: &Kind, now: &Kind) -> bool {
    matches!(
        (old, now),
        (Kind::Dir, Kind::Dir)
            | (Kind::File { .. }, Kind::File { .. })
            | (Kind::Link(_), Kind::Link(_))
    )
}

/// Whether every directory above `rel` is a real directory (not a link), so
/// that a write below it stays in the checkout.
fn parents_are_dirs(checkout: &Path, rel: &Path) -> bool {
    let mut above = PathBuf::new();
    let parts: Vec<_> = rel.components().collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if let Component::Normal(name) = part {
            above.push(name);
        }
        let real = std::fs::symlink_metadata(checkout.join(&above)).is_ok_and(|m| m.is_dir());
        if !real {
            return false;
        }
    }
    true
}

/// Put the metadata back to the copy: remove what the turn added or replaced
/// (deepest first, without following links), then restore each copied entry
/// that is missing or differs (parents first). Returns what could not be put
/// back.
fn restore(checkout: &Path, before: &Tree, after: &Tree) -> Vec<String> {
    let mut problems = Vec::new();
    for (rel, now) in after.iter().rev() {
        let stale = match before.get(rel) {
            None => true,
            Some(old) => {
                matches!(now.node.kind, Kind::Other) || !same_kind(&old.node.kind, &now.node.kind)
            }
        };
        if !stale {
            continue;
        }
        let path = checkout.join(rel);
        let removed = if matches!(now.node.kind, Kind::Dir) {
            std::fs::remove_dir(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = removed {
            problems.push(format!("could not remove {}: {e}", rel.display()));
        }
    }
    for (rel, old) in before {
        let unchanged = after.get(rel).is_some_and(|now| now.node == old.node);
        if unchanged {
            continue;
        }
        if !parents_are_dirs(checkout, rel) {
            problems.push(format!(
                "could not restore {}: a parent is not a directory",
                rel.display()
            ));
            continue;
        }
        if let Err(e) = put_back(&checkout.join(rel), old) {
            problems.push(format!("could not restore {}: {e}", rel.display()));
        }
    }
    problems
}

/// Create one copied entry at `path`, or set its mode when only that changed.
fn put_back(path: &Path, saved: &Saved) -> Result<(), String> {
    let mode = std::fs::Permissions::from_mode(saved.node.mode);
    match &saved.node.kind {
        Kind::Dir => {
            let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
            if !is_dir {
                std::fs::create_dir(path).map_err(|e| e.to_string())?;
            }
            std::fs::set_permissions(path, mode).map_err(|e| e.to_string())
        }
        Kind::Link(target) => {
            if std::fs::symlink_metadata(path).is_ok() {
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
            }
            std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string())
        }
        Kind::File { .. } => match &saved.bytes {
            Some(bytes) => {
                ap_core::fsx::write_atomic(path, bytes).map_err(|e| e.to_string())?;
                std::fs::set_permissions(path, mode).map_err(|e| e.to_string())
            }
            None => {
                // Not kept: only a mode change can be undone, and only when the
                // length and digest still match the copy.
                let now = std::fs::symlink_metadata(path).ok().filter(|m| m.is_file());
                let same = matches!(
                    (&now, &saved.node.kind),
                    (Some(m), Kind::File { len, digest: None }) if m.len() == *len
                ) || matches!(
                    (&now, &saved.node.kind),
                    (Some(m), Kind::File { len, digest: Some(d) })
                        if m.len() == *len && stream_digest(path).as_ref() == Some(d)
                );
                if same {
                    std::fs::set_permissions(path, mode).map_err(|e| e.to_string())
                } else {
                    Err("its contents were not kept (over 16 MiB, or a git object), so they cannot be put back".into())
                }
            }
        },
        Kind::Other => Err("not a file, directory or link".into()),
    }
}

/// The copy taken when a turn starts, and the check when it ends.
pub(super) struct MetadataGuard {
    checkout: Option<PathBuf>,
    before: Tree,
}

impl MetadataGuard {
    /// Copy the metadata of the turn's checkout, if it has one.
    pub(super) fn begin(checkout: Option<&Path>) -> Self {
        let checkout = checkout.map(Path::to_path_buf);
        let before = checkout
            .as_deref()
            .map(|c| scan(c, true))
            .unwrap_or_default();
        Self { checkout, before }
    }

    /// Compare the metadata with the copy and put it back when it changed.
    /// Returns what changed when the turn must be rejected, `None` otherwise.
    pub(super) fn settle(self) -> Option<String> {
        let checkout = self.checkout?;
        let after = scan(&checkout, false);
        let changed = changes(&self.before, &after);
        if changed.is_empty() {
            return None;
        }
        let problems = restore(&checkout, &self.before, &after);
        let mut named: Vec<String> = changed
            .iter()
            .take(NAMED_CHANGES)
            .map(|(p, what)| format!("{what} {}", p.display()))
            .collect();
        if changed.len() > NAMED_CHANGES {
            named.push(format!("and {} more", changed.len() - NAMED_CHANGES));
        }
        let mut detail = format!(
            "modified osc/git metadata of the checkout ({})",
            named.join(", ")
        );
        if !problems.is_empty() {
            detail.push_str("; ");
            detail.push_str(&problems.join("; "));
        }
        Some(detail)
    }
}
