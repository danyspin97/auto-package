//! The changes an update crosses, read from the changelog file that ships in
//! the new source archive.

use std::path::Path;

/// The changelog file names looked for at the top of the source tree, in the
/// order they are preferred.
const NAMES: &[&str] = &[
    "NEWS",
    "NEWS.md",
    "NEWS.txt",
    "NEWS.rst",
    "CHANGELOG",
    "CHANGELOG.md",
    "CHANGELOG.txt",
    "CHANGELOG.rst",
    "changelog.md",
    "ChangeLog",
    "Changelog.md",
    "CHANGES",
    "CHANGES.md",
    "CHANGES.txt",
    "CHANGES.rst",
    "HISTORY",
    "HISTORY.md",
    "HISTORY.rst",
    "RELEASES.md",
    "RELEASE_NOTES.md",
];
/// A changelog bigger than this is not read.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// The section kept is cut to this many characters.
const MAX_SECTION_CHARS: usize = 20_000;

/// The part of a changelog between two versions, with the file it came from.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Section {
    pub(super) file: String,
    pub(super) text: String,
}

/// The changelog of the source archive at `archive`, cut to what `new` added
/// after `old`. `None` when the archive has no changelog file at its top or
/// the file does not say where `old` and `new` are.
pub(super) fn section_of_archive(archive: &Path, old: &str, new: &str) -> Option<Section> {
    let dir = tempfile::tempdir().ok()?;
    let dest = dir.path().join("src");
    // Only a changelog file at the top-level directory, or in a `docs` or
    // `doc` directory at most one level below it, is unpacked; the rest of
    // the archive is read past and dropped.
    ap_ecosystem::sdist::extract_where(archive, &dest, |p| {
        let parts: Vec<_> = p.components().collect();
        let part = |i: usize| parts[i].as_os_str().to_string_lossy().into_owned();
        (2..=4).contains(&parts.len())
            && NAMES.contains(&part(parts.len() - 1).as_str())
            && (parts.len() == 2
                || ["docs", "doc"].contains(&part(parts.len() - 2).to_lowercase().as_str()))
    })
    .ok()?;
    let top = std::fs::read_dir(&dest).ok()?.flatten().next()?.path();
    // The top of the tree first, then its `docs`, then one directory down.
    let mut dirs = vec![top.clone(), top.join("docs"), top.join("doc")];
    if let Ok(entries) = std::fs::read_dir(&top) {
        let mut subs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        subs.sort();
        for sub in subs {
            dirs.extend([sub.join("docs"), sub.join("doc")]);
        }
    }
    dirs.iter().find_map(|dir| {
        NAMES.iter().find_map(|name| {
            let path = dir.join(name);
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                return None;
            }
            let bytes = std::fs::read(&path).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            section_between(&text, old, new).map(|text| Section {
                file: (*name).to_string(),
                text,
            })
        })
    })
}

/// The lines of `changelog` above the heading of `old`, when `new` has a
/// heading above it (the newest release first, as nearly every changelog is
/// kept). `None` when either heading is missing or nothing lies between them.
pub(super) fn section_between(changelog: &str, old: &str, new: &str) -> Option<String> {
    let lines: Vec<&str> = changelog.lines().collect();
    let old_at = lines.iter().position(|l| is_heading_of(l, old))?;
    let new_at = lines[..old_at].iter().position(|l| is_heading_of(l, new))?;
    let section: String = lines[new_at..old_at].join("\n");
    let section = section.trim();
    if section.is_empty() {
        return None;
    }
    Some(section.chars().take(MAX_SECTION_CHARS).collect())
}

/// Whether `line` is a heading for `version`: a short line that names the
/// version as a whole token (`## 3.3.0`, `Version 3.3.0 (2020-01-01)`,
/// `v3.3.0:`), not a bullet that mentions it.
fn is_heading_of(line: &str, version: &str) -> bool {
    let t = line.trim();
    if t.len() > 80
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || version.is_empty()
    {
        return false;
    }
    let mut from = 0;
    while let Some(i) = t[from..].find(version) {
        let at = from + i;
        let end = at + version.len();
        let before = t[..at].chars().next_back();
        let after = t[end..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
        // `1.2` is not the heading of `1.2.3`, nor `2.3` of `12.3`.
        let continues = after.is_some_and(|c| c == '.')
            && t[end + 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
        if !word(before) && !word(after) && !continues && before != Some('.') {
            return true;
        }
        from = end;
    }
    false
}
