//! The model's per-release recap of a jump, read back and checked before it is
//! used: the answer must name each release that has notes, in order and with
//! nothing invented, list only bullets within the limits, and keep every
//! security reference of the notes. Anything else is refused, and the
//! releases are shrunk mechanically instead.

use std::collections::BTreeSet;

use super::caps::{BULLET_CHARS, PER_RELEASE, TOTAL, head, is_security, rank};
use super::per_release::Release;

/// The most characters of the notes the model is given.
const MAX_NOTES_CHARS: usize = 20_000;

/// The notes of `releases` as the model reads them: a `## <version>` section
/// per release with notes, newest first, each with its bullets as `- ` lines
/// or its prose.
pub(super) fn notes_of(releases: &[Release]) -> String {
    let notes: String = releases
        .iter()
        .filter(|r| r.has_notes())
        .map(|r| {
            let body = match &r.prose {
                Some(text) => text.trim().to_string(),
                None => r.bullets.iter().map(|b| format!("- {b}\n")).collect(),
            };
            format!("## {}\n{}\n", r.version, body)
        })
        .collect::<String>();
    head(&notes, MAX_NOTES_CHARS)
}

/// The CVE, bsc#, boo# and gh# references in `text`, lowercased.
pub(super) fn security_refs(text: &str) -> BTreeSet<String> {
    let lower = text.to_ascii_lowercase();
    let mut refs = BTreeSet::new();
    for marker in ["cve-", "bsc#", "boo#", "gh#"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(marker) {
            let start = from + at;
            let len = lower[start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '#')))
                .unwrap_or(lower.len() - start);
            let token = lower[start..start + len].trim_end_matches(['-', '#', '/']);
            refs.insert(token.to_string());
            from = start + len;
        }
    }
    refs
}

/// The lists of the releases with notes of `releases` (in their order) as the
/// model's `answer` gives them, or the reason the answer is refused. Each list
/// is in the order a release lists its bullets: breaking changes first, then
/// security fixes, then the rest.
pub(super) fn checked(
    releases: &[Release],
    answer: &str,
) -> Result<Vec<Vec<String>>, &'static str> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for raw in answer.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(version) = line.strip_prefix("## ") {
            sections.push((version.trim().to_string(), Vec::new()));
            continue;
        }
        let Some(item) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- ")) else {
            return Err("a line is neither a release heading nor a bullet");
        };
        let item = item.split_whitespace().collect::<Vec<_>>().join(" ");
        if !item.chars().any(char::is_alphanumeric) {
            return Err("an empty bullet");
        }
        sections
            .last_mut()
            .ok_or("a bullet comes before any release")?
            .1
            .push(item);
    }
    let expected: Vec<&Release> = releases.iter().filter(|r| r.has_notes()).collect();
    if sections.len() != expected.len() {
        return Err("the releases of the answer are not the releases with notes");
    }
    let mut lists = Vec::with_capacity(sections.len());
    for ((version, items), release) in sections.into_iter().zip(&expected) {
        if version != release.version {
            return Err("a release heading is not the next release with notes");
        }
        if items.is_empty() {
            return Err("a release has no bullet");
        }
        if items.len() > PER_RELEASE {
            return Err("a release has more bullets than its limit");
        }
        if items
            .iter()
            .any(|i| !is_security(i) && i.chars().count() > BULLET_CHARS)
        {
            return Err("a bullet is longer than its limit");
        }
        lists.push(items);
    }
    if lists.iter().map(Vec::len).sum::<usize>() > TOTAL {
        return Err("the answer has more bullets than the total limit");
    }
    if !security_refs(&notes_of(releases)).is_subset(&security_refs(answer)) {
        return Err("a CVE, bsc#, boo# or gh# reference of the notes is missing");
    }
    for list in &mut lists {
        list.sort_by_key(|i| rank(i));
    }
    Ok(lists)
}
