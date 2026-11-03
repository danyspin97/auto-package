//! The second proof that a patch's change is already in the new sources: every
//! hunk of the patch adds lines that the file it changes already has in the new
//! source archive. It needs no quilt, so it also judges a patch quilt did not run on.

use std::path::{Path, PathBuf};

/// One hunk of a unified diff: the file it changes (`None` when the file is
/// created or deleted, which the proof does not cover) and the lines it adds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) file: Option<String>,
    pub(crate) added: Vec<String>,
}

/// Whether a line can prove anything: blank lines, lines of punctuation only
/// (`}`, `{`, `);`) and short ones (`break;`, `return 0;`) are in almost every
/// file, so a line needs eight letters or digits.
fn is_substantive(line: &str) -> bool {
    line.chars().filter(|c| c.is_alphanumeric()).count() >= 8
}

/// Whether `needle` occurs in `lines` as one contiguous run, in order.
fn contains_run(lines: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && lines
            .windows(needle.len())
            .any(|w| w.iter().zip(needle).all(|(a, b)| a == b))
}

/// Whether every hunk of `diff` adds a substantive line, and its added lines
/// occur in its file of the new archive `tarball` as one contiguous run, in
/// order. `level` is the `-pN` the spec applies the patch with: a file's name
/// is its `+++` name without its first `level` components. False when the
/// archive does not unpack or a file is missing.
pub fn already_in_tarball(tarball: &Path, diff: &[u8], level: u32) -> bool {
    let hunks = hunks(&String::from_utf8_lossy(diff), level);
    if hunks.is_empty()
        || hunks
            .iter()
            .any(|h| h.file.is_none() || !h.added.iter().any(|l| is_substantive(l)))
    {
        return false;
    }
    let targets: Vec<String> = hunks.iter().filter_map(|h| h.file.clone()).collect();
    let Ok(scratch) = tempfile::tempdir() else {
        return false;
    };
    let src = scratch.path().join("src");
    let wanted = |p: &Path| targets.iter().any(|t| p.ends_with(t));
    if ap_ecosystem::sdist::extract_where(tarball, &src, wanted).is_err() {
        return false;
    }
    let root = top_dir(&src);
    hunks.iter().all(|h| {
        let Some(file) = &h.file else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            return false;
        };
        let lines: Vec<String> = text
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        let added: Vec<String> = h
            .added
            .iter()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        contains_run(&lines, &added)
    })
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
/// taken for a file header. A file's name is its `+++` name without its first
/// `level` components, as `patch -pN` strips it.
pub(crate) fn hunks(diff: &str, level: u32) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut file: Option<String> = None;
    // Lines still to come in the current hunk: old and new side.
    let mut left: Option<(usize, usize)> = None;
    for line in diff.lines() {
        if let Some((old, new)) = left {
            let (old, new) = match line.as_bytes().first() {
                Some(b'+') => {
                    if let Some(h) = out.last_mut() {
                        h.added.push(line[1..].to_string());
                    }
                    (old, new.saturating_sub(1))
                }
                Some(b'-') => (old.saturating_sub(1), new),
                Some(b'\\') => (old, new),
                _ => (old.saturating_sub(1), new.saturating_sub(1)),
            };
            left = (old > 0 || new > 0).then_some((old, new));
            continue;
        }
        if line.starts_with("diff ") {
            file = None;
        } else if let Some(name) = line.strip_prefix("+++ ") {
            file = target_of(name, level);
        } else if line.starts_with("@@") {
            let counts = hunk_counts(line);
            out.push(Hunk {
                file: file.clone(),
                added: Vec::new(),
            });
            // A header that does not parse leaves its hunk without lines, so no
            // proof is made from it.
            left = counts.filter(|&(o, n)| o > 0 || n > 0);
        }
    }
    out
}

/// The file a `+++` name changes once `level` leading components are stripped
/// (`patch -pN`; empty components from `//` do not count). `None` for
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
