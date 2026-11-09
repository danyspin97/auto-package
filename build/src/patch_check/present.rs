//! The second proof that a patch's change is already in the new sources: each
//! hunk of the patch is in the new source archive. It needs no quilt, so it also
//! judges a patch quilt did not run on.
//!
//! A hunk that adds lines is proven by its post-image (see [`super::post_image`]):
//! its context and added lines, in hunk order, are one contiguous run of its
//! file. A hunk that only removes lines is proven by its context being one run
//! of the file, with its removed lines not between the context lines. A hunk
//! that deletes a whole file is proven when that file is gone from the archive.

use std::path::{Path, PathBuf};

/// One line of a hunk, without its leading marker, in hunk order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HunkLine {
    /// A line the hunk keeps around its change.
    Context(String),
    /// A line the hunk adds.
    Added(String),
    /// A line the hunk removes.
    Removed(String),
}

impl HunkLine {
    pub(crate) fn text(&self) -> &str {
        match self {
            HunkLine::Context(t) | HunkLine::Added(t) | HunkLine::Removed(t) => t.as_str(),
        }
    }
}

/// One hunk of a unified diff, with the file it changes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hunk {
    /// The `+++` name without its `-pN` components; `None` when the hunk
    /// deletes the file.
    pub(crate) file: Option<String>,
    /// The `---` name without its `-pN` components; `None` when the hunk
    /// creates the file.
    pub(crate) old: Option<String>,
    /// The hunk's lines, in the order the diff gives them.
    pub(crate) lines: Vec<HunkLine>,
}

/// Whether a line can prove anything: blank lines, lines of punctuation only
/// (`}`, `{`, `);`) and short ones (`break;`, `return 0;`) are in almost every
/// file, so a line needs eight letters or digits.
pub(super) fn is_substantive(line: &str) -> bool {
    line.chars().filter(|c| c.is_alphanumeric()).count() >= 8
}

/// A line without the carriage return a CRLF file or patch leaves on it.
fn trimmed(line: &str) -> String {
    line.trim_end_matches('\r').to_string()
}

/// Whether every hunk of `diff` is in the new archive `tarball` (see the module
/// doc). `level` is the `-pN` the spec applies the patch with: a file's name is
/// its `+++` or `---` name without its first `level` components. False when the
/// archive does not unpack, a file is missing or a hunk is not proven.
pub fn already_in_tarball(tarball: &Path, diff: &[u8], level: u32) -> bool {
    let hunks = hunks(&String::from_utf8_lossy(diff), level);
    let Some((_scratch, root)) = unpack_targets(tarball, &hunks) else {
        return false;
    };
    hunks.iter().all(|h| hunk_in_tree(&root, h))
}

/// Whether every file `diff` changes is gone from the new archive `tarball`:
/// none of the hunks' `+++` and `---` targets is in it. Only that is judged: a
/// patch whose files are all gone cannot apply, and nothing says the spec no
/// longer needs it. Same `level` and false cases as [`already_in_tarball`].
pub fn targets_absent_in_tarball(tarball: &Path, diff: &[u8], level: u32) -> bool {
    let hunks = hunks(&String::from_utf8_lossy(diff), level);
    let Some((_scratch, root)) = unpack_targets(tarball, &hunks) else {
        return false;
    };
    targets_of(&hunks)
        .iter()
        .all(|t| std::fs::symlink_metadata(root.join(t)).is_err())
}

/// The names the hunks change, in hunk order: `+++` and `---`, without `/dev/null`.
fn targets_of(hunks: &[Hunk]) -> Vec<String> {
    hunks
        .iter()
        .flat_map(|h| [h.file.clone(), h.old.clone()])
        .flatten()
        .collect()
}

/// The archive unpacked, with only the hunks' targets, and the directory it
/// unpacked into. None when the hunks name no file, a hunk names neither side,
/// or the archive does not unpack.
fn unpack_targets(tarball: &Path, hunks: &[Hunk]) -> Option<(tempfile::TempDir, PathBuf)> {
    if hunks.is_empty() || hunks.iter().any(|h| h.file.is_none() && h.old.is_none()) {
        return None;
    }
    let targets = targets_of(hunks);
    let scratch = tempfile::tempdir().ok()?;
    let src = scratch.path().join("src");
    let wanted = |p: &Path| targets.iter().any(|t| p.ends_with(t));
    ap_ecosystem::sdist::extract_where(tarball, &src, wanted).ok()?;
    let root = top_dir(&src);
    Some((scratch, root))
}

/// Whether one hunk is in the tree at `root`: a deletion when the file is gone,
/// else the hunk's post-image is in the file (see [`super::post_image`]).
fn hunk_in_tree(root: &Path, hunk: &Hunk) -> bool {
    let Some(file) = &hunk.file else {
        return hunk
            .old
            .as_deref()
            .is_some_and(|old| std::fs::symlink_metadata(root.join(old)).is_err());
    };
    let Ok(text) = std::fs::read_to_string(root.join(file)) else {
        return false;
    };
    let lines: Vec<String> = text.lines().map(trimmed).collect();
    if hunk.lines.iter().any(|l| matches!(l, HunkLine::Added(_))) {
        super::post_image::added_in_file(&lines, hunk)
    } else {
        super::post_image::removal_in_file(&lines, hunk)
    }
}

/// The one directory an archive unpacks into, or `src` itself.
fn top_dir(src: &Path) -> PathBuf {
    let entries: Vec<_> = std::fs::read_dir(src)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => src.to_path_buf(),
    }
}

/// The hunks of a unified diff. A hunk ends after the number of old and new
/// lines its `@@` header gives, so a removed line that starts with `---` is not
/// taken for a file header. A file's name is its `+++` or `---` name without its
/// first `level` components, as `patch -pN` strips it.
pub(crate) fn hunks(diff: &str, level: u32) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut file: Option<String> = None;
    let mut old: Option<String> = None;
    // Lines still to come in the current hunk: old and new side.
    let mut left: Option<(usize, usize)> = None;
    for line in diff.lines() {
        if let Some((o, n)) = left {
            let (o, n, kept) = match line.as_bytes().first() {
                Some(b'+') => (
                    o,
                    n.saturating_sub(1),
                    Some(HunkLine::Added(trimmed(&line[1..]))),
                ),
                Some(b'-') => (
                    o.saturating_sub(1),
                    n,
                    Some(HunkLine::Removed(trimmed(&line[1..]))),
                ),
                Some(b'\\') => (o, n, None),
                _ => {
                    let text = trimmed(line.get(1..).unwrap_or_default());
                    (
                        o.saturating_sub(1),
                        n.saturating_sub(1),
                        Some(HunkLine::Context(text)),
                    )
                }
            };
            if let (Some(h), Some(l)) = (out.last_mut(), kept) {
                h.lines.push(l);
            }
            left = (o > 0 || n > 0).then_some((o, n));
            continue;
        }
        if line.starts_with("diff ") {
            file = None;
            old = None;
        } else if let Some(name) = line.strip_prefix("--- ") {
            old = target_of(name, level);
        } else if let Some(name) = line.strip_prefix("+++ ") {
            file = target_of(name, level);
        } else if line.starts_with("@@") {
            let counts = hunk_counts(line);
            out.push(Hunk {
                file: file.clone(),
                old: old.clone(),
                lines: Vec::new(),
            });
            // A header that does not parse leaves its hunk without lines, so no
            // proof is made from it.
            left = counts.filter(|&(o, n)| o > 0 || n > 0);
        }
    }
    out
}

/// The file a `+++` or `---` name changes once `level` leading components are
/// stripped (`patch -pN`; empty components from `//` do not count). `None` for
/// `/dev/null`, a name with fewer components than `level` + 1, an absolute
/// result and a `..` component: none of them names a file of the archive.
fn target_of(name: &str, level: u32) -> Option<String> {
    let name = name.split('\t').next().unwrap_or("").trim();
    if name == "/dev/null" || name.is_empty() {
        return None;
    }
    let parts: Vec<&str> = name.split('/').filter(|c| !c.is_empty()).collect();
    let kept = parts.get(level as usize..)?;
    if kept.is_empty() || kept.iter().any(|c| *c == ".." || *c == ".") {
        return None;
    }
    Some(kept.join("/"))
}

/// The old and new line counts of a `@@ -a[,b] +c[,d] @@` header.
fn hunk_counts(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace().skip(1);
    let count = |tok: &str| match tok.split_once(',') {
        Some((_, n)) => n.parse().ok(),
        None => Some(1),
    };
    let old = count(parts.next()?.strip_prefix('-')?)?;
    let new = count(parts.next()?.strip_prefix('+')?)?;
    Some((old, new))
}
