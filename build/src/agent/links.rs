//! The symbolic links a turn leaves in the trees it works in. quilt applies a
//! patch that creates a link (`new file mode 120000`), so a turn can make
//! `source/lnk` point into the checkout's `.osc`, and a write through it is a
//! write to the metadata while the textual denies see only `source/lnk/...`.
//! Each link the turn created or retargeted is resolved after it: one that
//! reaches the checkout's `.osc` or `.git`, or anything outside the workspace
//! and the checkout, is undone and the turn rejected. Links that were there
//! before the turn are the pipeline's and are not checked.
//!
//! Whether Claude Code's own permission matching sees the real path of a
//! symlinked file is not known here; this check does not depend on it.

use super::tools::{CHECKOUT_METADATA_DIRS, REFRESH_SERIES};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Links followed in one resolution, in total, before it is treated as escaping
/// (a loop, or a chain that fans out). A total, not a depth: a link whose target
/// names two links that each name two more takes exponential time otherwise.
const MAX_LINK_FOLLOWS: u32 = 40;

/// Every symbolic link under the walked trees, keyed by its path (canonical,
/// since the walk does not follow links) and holding its target as written.
type Links = BTreeMap<PathBuf, PathBuf>;

/// Walk `dir` without following links, recording each link. `skip` names
/// directories not to enter.
fn walk(dir: &Path, skip: &[PathBuf], out: &mut Links) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if skip.contains(&path) {
            continue;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                out.insert(path, target);
            }
        } else if ft.is_dir() {
            walk(&path, skip, out);
        }
    }
}

/// The physical path `path` names when resolved from `start`, following each
/// link the way the kernel does (a `..` after a link goes to the link's target's
/// parent). `None` when the links loop, or when more than [`MAX_LINK_FOLLOWS`]
/// are followed. A component that does not exist is taken as a plain name.
fn resolve(start: PathBuf, path: &Path) -> Option<PathBuf> {
    resolve_counted(start, path, &mut 0)
}

fn resolve_counted(start: PathBuf, path: &Path, follows: &mut u32) -> Option<PathBuf> {
    let mut cur = start;
    for comp in path.components() {
        match comp {
            Component::RootDir => cur = PathBuf::from("/"),
            Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(name) => {
                let candidate = cur.join(name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(m) if m.file_type().is_symlink() => {
                        *follows += 1;
                        if *follows > MAX_LINK_FOLLOWS {
                            return None;
                        }
                        let target = std::fs::read_link(&candidate).ok()?;
                        cur = resolve_counted(cur.clone(), &target, follows)?;
                    }
                    _ => cur = candidate,
                }
            }
        }
    }
    Some(cur)
}

/// The links of the turn's trees before the turn, and the check after it.
pub(super) struct LinkGuard {
    /// Canonical directories walked: the workspace, its `source/` (a link to
    /// the sdist when it is one), and the checkout.
    walked: Vec<(PathBuf, Vec<PathBuf>)>,
    /// Where a link may resolve: the workspace's `source/` and the checkout. Not
    /// the workspace itself: its own files (state, notes, the OBS baseline) are
    /// the pipeline's.
    allowed: Vec<PathBuf>,
    /// Where it may never resolve: the checkout's metadata. A link may not
    /// resolve to a directory above it either (the checkout root), since a path
    /// through that link reaches the metadata.
    forbidden: Vec<PathBuf>,
    /// The checkout, whose quilt series and changes files a link may not reach.
    checkout: Option<PathBuf>,
    before: Links,
}

fn canonical_or(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `resolved` is a file of the checkout the pipeline or quilt write
/// (the series files, and the `.changes` files whose text reaches the commit).
fn is_checkout_control_file(checkout: &Path, resolved: &Path) -> bool {
    if !resolved.starts_with(checkout) {
        return false;
    }
    let Some(name) = resolved.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name == "series" || name == REFRESH_SERIES || name.ends_with(".changes")
}

impl LinkGuard {
    /// Record the links of the turn's trees.
    pub(super) fn begin(workspace: &Path, checkout: Option<&Path>) -> Self {
        let ws = canonical_or(workspace);
        let source = canonical_or(&ws.join("source"));
        let mut walked = vec![(ws.clone(), Vec::new())];
        let mut allowed = Vec::new();
        if source != ws {
            walked.push((source.clone(), Vec::new()));
            allowed.push(source);
        }
        let mut forbidden = Vec::new();
        let mut checkout_root = None;
        if let Some(co) = checkout {
            let co = canonical_or(co);
            let skip: Vec<PathBuf> = CHECKOUT_METADATA_DIRS.iter().map(|d| co.join(d)).collect();
            forbidden.extend(skip.iter().cloned());
            walked.push((co.clone(), skip));
            allowed.push(co.clone());
            checkout_root = Some(co);
        }
        let mut guard = Self {
            walked,
            allowed,
            forbidden,
            checkout: checkout_root,
            before: Links::new(),
        };
        guard.before = guard.links_now();
        guard
    }

    fn links_now(&self) -> Links {
        let mut out = Links::new();
        for (root, skip) in &self.walked {
            walk(root, skip, &mut out);
        }
        out
    }

    /// Whether a link at `path` with `target` resolves inside the allowed trees
    /// and nowhere the checkout's metadata is, nor a directory above it.
    fn stays_inside(&self, path: &Path, target: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let Some(resolved) = resolve(parent.to_path_buf(), target) else {
            return false;
        };
        self.allowed.iter().any(|a| resolved.starts_with(a))
            && !self
                .forbidden
                .iter()
                .any(|f| f.starts_with(&resolved) || resolved.starts_with(f))
            && !self
                .checkout
                .as_deref()
                .is_some_and(|co| is_checkout_control_file(co, &resolved))
    }

    /// Undo each link the turn created or retargeted that does not stay inside
    /// the trees: the link is removed, or put back with its old target. Returns
    /// a description of each one undone.
    pub(super) fn settle(self) -> Vec<String> {
        let after = self.links_now();
        // Every link is judged while all of them are still in place: one link
        // can resolve through another that the turn made.
        let bad: Vec<(&PathBuf, &PathBuf)> = after
            .iter()
            .filter(|(path, target)| {
                self.before.get(*path) != Some(*target) && !self.stays_inside(path, target)
            })
            .collect();
        let mut undone = Vec::new();
        for (path, target) in bad {
            let old = self.before.get(path);
            let mut note = format!("{} -> {}", path.display(), target.display());
            if let Err(e) = std::fs::remove_file(path) {
                note.push_str(&format!(" (could not be removed: {e})"));
            } else if let Some(old) = old
                && let Err(e) = std::os::unix::fs::symlink(old, path)
            {
                note.push_str(&format!(" (could not be put back: {e})"));
            }
            undone.push(note);
        }
        undone
    }
}
