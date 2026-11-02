//! The unpublished entry of a `.changes` file, rewritten from the net change:
//! bullets that name the same package file become one, and a bullet that names
//! only files the final diff leaves alone is dropped. No model call: a merged
//! bullet keeps the text of the first bullet that is not a refresh, unless a
//! later one starts with Revert, Replace, Instead, Undo or Drop.

use super::changes::newest_changes_file;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// One bullet of an entry body.
struct Bullet {
    /// The lines as written: the bullet line, then its continuation lines.
    lines: Vec<String>,
    /// The bullet line's text, without the `- `.
    head: String,
    /// `head` with the continuation lines joined on it.
    full: String,
    /// The package files the whole text names, in order, once each.
    names: Vec<String>,
}

/// A part of an entry body: a bullet, or any other line, kept as it is.
enum Part {
    Bullet(Bullet),
    Other(String),
}

/// What the rewrite does with one part.
enum Fate {
    Keep,
    Drop,
    /// Joins the bullets whose first named file is `key`.
    Group(String),
}

fn is_separator(l: &str) -> bool {
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// `changes` (the text of a `.changes` file) with its newest entry rewritten
/// from the net change. `pristine` is what OBS holds: an entry it already has is
/// left alone. `changed` holds the package files the final diff touches, `known`
/// every package file. A bullet naming several files joins the group of the
/// first one, and is dropped only when none of them changed.
///
/// `None` when nothing changes, or when the entry has no `Update to <version>`
/// bullet (an entry that is not a version update is left alone).
pub fn rewrite_entry_from_diff(
    changes: &str,
    pristine: &str,
    changed: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Option<String> {
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    if pristine.contains(lines[first..end].join("\n").trim_end()) {
        return None;
    }
    // The header line (date - author) is the one after the separator.
    let body_from = (first + 2).min(end);
    let parts = parse_body(&lines[body_from..end], known);
    let has_version = parts.iter().any(|p| match p {
        Part::Bullet(b) => update_version(&b.head).is_some(),
        _ => false,
    });
    if !has_version {
        return None;
    }
    let fates = fates(&parts, changed);
    // Per group (the file it names first): its bullets, in order, and the one
    // whose text it keeps.
    let mut groups: BTreeMap<&str, Vec<(usize, &Bullet)>> = BTreeMap::new();
    for (i, (part, fate)) in parts.iter().zip(&fates).enumerate() {
        if let (Part::Bullet(b), Fate::Group(key)) = (part, fate) {
            groups.entry(key.as_str()).or_default().push((i, b));
        }
    }
    let kept: BTreeMap<&str, usize> = groups
        .iter()
        .map(|(key, members)| (*key, members[representative(members)].0))
        .collect();
    let mut body: Vec<String> = Vec::new();
    for (i, (part, fate)) in parts.iter().zip(&fates).enumerate() {
        match (part, fate) {
            (Part::Other(line), _) => push_line(&mut body, line),
            (Part::Bullet(_), Fate::Drop) => {}
            (Part::Bullet(b), Fate::Keep) => body.extend(b.lines.iter().cloned()),
            (Part::Bullet(_), Fate::Group(key)) => {
                // The group's text goes where its first bullet stood.
                if groups[key.as_str()][0].0 == i
                    && let Part::Bullet(chosen) = &parts[kept[key.as_str()]]
                {
                    body.extend(chosen.lines.iter().cloned());
                }
            }
        }
    }
    let mut out: Vec<String> = lines[..body_from].iter().map(|l| l.to_string()).collect();
    out.extend(body);
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    (text.trim_end() != changes.trim_end()).then_some(text)
}

/// The position, in `group`, of the bullet whose text a merged bullet keeps. It
/// is the first bullet that is not a refresh; a file whose bullets are all
/// refreshes keeps its first one. A later bullet that starts with one of
/// `REPLACING` takes over from it, the last such bullet winning.
fn representative(group: &[(usize, &Bullet)]) -> usize {
    let Some(first) = group.iter().position(|(_, b)| !is_refresh(&b.head)) else {
        return 0;
    };
    group[first..]
        .iter()
        .rposition(|(_, b)| REPLACING.iter().any(|w| b.head.starts_with(w)))
        .map_or(first, |k| first + k)
}

/// Whether a bullet is a quilt refresh of a patch: `Refresh <patch>`.
fn is_refresh(head: &str) -> bool {
    head.starts_with("Refresh ")
}

/// The leading words of a bullet that take over the text of the bullets before
/// them: the change it makes undoes or replaces what they say.
const REPLACING: [&str; 5] = ["Revert", "Replace", "Instead", "Undo", "Drop"];

/// [`rewrite_entry_from_diff`] on the checkout's `.changes` file (the one
/// [`newest_changes_file`] picks), against `baseline`: the directory
/// `save_baseline` kept, holding what OBS had before this run (`files/` holds
/// each name's copy, `tracked` the names). Best effort: a file that cannot be
/// read or written is left as it is.
pub fn rewrite_unpublished_entry(checkout_dir: &Path, baseline: &Path) {
    let Some(file) = newest_changes_file(checkout_dir) else {
        return;
    };
    let Some(name) = file.file_name() else {
        return;
    };
    let Ok(working) = fs::read_to_string(&file) else {
        return;
    };
    let pristine = fs::read_to_string(baseline.join("files").join(name)).unwrap_or_default();
    let (changed, known) = package_diff(checkout_dir, baseline);
    if let Some(text) = rewrite_entry_from_diff(&working, &pristine, &changed, &known) {
        let _ = fs::write(&file, text);
    }
}

/// The package files the checkout changes against `baseline`, and every package
/// file: the names the baseline tracks and the checkout holds (hidden files
/// aside). A file counts as changed when it is missing on one side.
fn package_diff(checkout_dir: &Path, baseline: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let saved = baseline.join("files");
    let tracked = fs::read_to_string(baseline.join("tracked")).unwrap_or_default();
    let mut known: BTreeSet<String> = tracked
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && !name.starts_with('.') {
                known.insert(name);
            }
        }
    }
    let changed = known
        .iter()
        .filter(|n| differs(&checkout_dir.join(n), &saved.join(n)))
        .cloned()
        .collect();
    (changed, known)
}

/// Whether the file at `now` differs from the one at `before`: missing on one
/// side only, or of another length, or other bytes.
fn differs(now: &Path, before: &Path) -> bool {
    match (fs::metadata(now), fs::metadata(before)) {
        (Ok(a), Ok(b)) if a.len() != b.len() => true,
        (Ok(_), Ok(_)) => fs::read(now).ok() != fs::read(before).ok(),
        (Err(_), Err(_)) => false,
        _ => true,
    }
}

/// `line` onto `body`, unless it is blank and `body` already ends blank.
fn push_line(body: &mut Vec<String>, line: &str) {
    let blank = line.trim().is_empty();
    if !(blank && body.last().is_some_and(|l| l.trim().is_empty())) {
        body.push(line.to_string());
    }
}

/// The parts of an entry body: a bullet starts at column 0 with `- `, and the
/// lines indented under it continue it. Any other line is a part of its own.
fn parse_body(lines: &[&str], known: &BTreeSet<String>) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    for line in lines {
        if let Some(head) = line.strip_prefix("- ") {
            let head = head.trim().to_string();
            parts.push(Part::Bullet(Bullet {
                lines: vec![line.to_string()],
                full: head.clone(),
                head,
                names: Vec::new(),
            }));
        } else if let Some(Part::Bullet(b)) = parts.last_mut()
            && line.starts_with(' ')
            && !line.trim().is_empty()
        {
            b.lines.push(line.to_string());
            b.full.push(' ');
            b.full.push_str(line.trim());
        } else {
            parts.push(Part::Other(line.to_string()));
        }
    }
    for part in &mut parts {
        if let Part::Bullet(b) = part {
            b.names = named_files(&b.full, known);
        }
    }
    parts
}

/// The words of `text` that are package files (`known`), once each, in order.
fn named_files(text: &str, known: &BTreeSet<String>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '"' | '\'' | '`' | ',' | ';' | ':' | '.'
            )
        });
        if known.contains(word) && !names.iter().any(|n| n == word) {
            names.push(word.to_string());
        }
    }
    names
}

/// The version of an `Update to <version>` bullet, by its first line only: the
/// bullet is kept whatever its continuation lines name (flags, notes).
fn update_version(head: &str) -> Option<&str> {
    head.strip_prefix("Update to ").filter(|v| !v.is_empty())
}

/// The fate of each part: bullets that name no file, and the `Update to` bullet,
/// stay; a bullet whose named files are all unchanged is dropped, and the rest
/// are grouped by their first.
fn fates(parts: &[Part], changed: &BTreeSet<String>) -> Vec<Fate> {
    parts
        .iter()
        .map(|p| match p {
            Part::Other(_) => Fate::Keep,
            Part::Bullet(b) if b.names.is_empty() || update_version(&b.head).is_some() => {
                Fate::Keep
            }
            Part::Bullet(b) if b.names.iter().all(|n| !changed.contains(n)) => Fate::Drop,
            Part::Bullet(b) => Fate::Group(b.names[0].clone()),
        })
        .collect()
}
