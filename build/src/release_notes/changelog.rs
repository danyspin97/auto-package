//! The changes an update crosses, read from the changelog file that ships in
//! the new source archive.

use std::cmp::Ordering;
use std::path::Path;

use super::markdown::strip_links;
use crate::version;

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
/// When `old` has no heading at all, the section is the top release only (see
/// [`section_from_top`]).
pub(super) fn section_between(changelog: &str, old: &str, new: &str) -> Option<String> {
    let lines: Vec<&str> = changelog.lines().collect();
    let section = match lines.iter().position(|l| is_heading_of(l, old)) {
        Some(old_at) => {
            let new_at = lines[..old_at].iter().position(|l| is_heading_of(l, new))?;
            lines[new_at..old_at].join("\n")
        }
        None => section_from_top(&lines, old, new)?,
    };
    let section = section.trim();
    if section.is_empty() {
        return None;
    }
    Some(section.chars().take(MAX_SECTION_CHARS).collect())
}

/// The text of `line` as a heading, without its links, or `None` when the line
/// is a bullet or too long for a heading. A `#` heading may run longer.
fn heading_text(line: &str) -> Option<String> {
    let t = strip_links(line.trim());
    let longest = if t.starts_with('#') { 200 } else { 80 };
    let bullet = ["- ", "* ", "+ "].iter().any(|m| t.starts_with(m));
    (t.len() <= longest && !bullet).then_some(t)
}

/// Whether `line` is a heading for `version`: a short line that names the
/// version as a whole token (`## 3.3.0`, `Version 3.3.0 (2020-01-01)`,
/// `v3.3.0:`, `## [v3.3.0](url) - 2020-01-01`), not a bullet that mentions it.
fn is_heading_of(line: &str, version: &str) -> bool {
    let Some(t) = heading_text(line) else {
        return false;
    };
    if version.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(i) = t[from..].find(version) {
        let at = from + i;
        let end = at + version.len();
        // In `v3.3.0` the `v` is a prefix, not a word before the version.
        let head = &t[..at];
        let head = match head.strip_suffix(['v', 'V']) {
            Some(rest) if !rest.ends_with(char::is_alphanumeric) => rest,
            _ => head,
        };
        let before = head.chars().next_back();
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

/// The releases of a changelog `section` (as [`section_between`] cuts it) that
/// are newer than `old` and not newer than `new`, in the section's order (the
/// newest first): each with its version and the body under its heading. Only
/// headings of the section's own level (its first release heading) split it.
pub(super) fn releases_of_section(section: &str, old: &str, new: &str) -> Vec<(String, String)> {
    let mut level = None;
    let mut out: Vec<(String, Vec<&str>)> = Vec::new();
    for line in section.lines() {
        let heading = heading_version(line);
        if let Some(v) = heading {
            let level = *level.get_or_insert_with(|| heading_level(line));
            let in_window = version::sort_cmp(&v, old) == Ordering::Greater
                && version::sort_cmp(&v, new) != Ordering::Greater;
            if heading_level(line) == level && in_window {
                out.push((v, Vec::new()));
                continue;
            }
        }
        if let Some(cur) = out.last_mut() {
            cur.1.push(line);
        }
    }
    out.into_iter()
        .map(|(v, body)| (v, body.join("\n")))
        .collect()
}

/// The section of a changelog whose heading for `old` is missing. It is taken
/// when `old` is older than every release heading in the file and `new` is the
/// first one: the section runs from that heading to the next release heading
/// of the same level, or to the end. `None` otherwise.
fn section_from_top(lines: &[&str], old: &str, new: &str) -> Option<String> {
    let releases: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| heading_version(l).map(|v| (i, v)))
        .collect();
    let (top, _) = releases.first()?;
    if !is_heading_of(lines[*top], new)
        || releases
            .iter()
            .any(|(_, v)| version::sort_cmp(old, v) != Ordering::Less)
    {
        return None;
    }
    let level = heading_level(lines[*top]);
    let end = releases[1..]
        .iter()
        .map(|(i, _)| *i)
        .find(|i| heading_level(lines[*i]) == level)
        .unwrap_or(lines.len());
    Some(lines[*top..end].join("\n"))
}

/// The version a heading line names: the first word of its first three that
/// is a dotted version, with an optional `v` (`## v3.3.0 - 2020`,
/// `Version 3.3.0`, `1.0.0 (2025-01-01)`). `None` for any other line.
fn heading_version(line: &str) -> Option<String> {
    heading_text(line)?
        .split_whitespace()
        .take(3)
        .find_map(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let word = word.strip_prefix(['v', 'V']).unwrap_or(word);
            let dotted = word.contains('.')
                && word.starts_with(|c: char| c.is_ascii_digit())
                && word.chars().all(|c| c.is_ascii_digit() || c == '.');
            dotted.then(|| word.to_string())
        })
}

/// The markdown level of a heading: its count of leading `#`, zero for a
/// heading without them (an underlined one).
fn heading_level(line: &str) -> usize {
    line.trim().chars().take_while(|c| *c == '#').count()
}
