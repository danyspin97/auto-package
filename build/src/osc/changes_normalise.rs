//! The unpublished entries of a branch package's `.changes`, normalised. An
//! entry the origin's `.changes` does not hold is this tool's own work, not
//! published history, and may be rewritten. An update entry (one with an
//! `Update to <version>` bullet) goes when its version is another release than
//! the spec's, or when the origin already publishes that version or a newer
//! unpublished entry names it; inside the others the bullets convention 40
//! forbids are dropped, duplicates are joined and `Refresh <x>.patch` bullets
//! are grouped. Any other unpublished entry is kept as it is. An entry the
//! origin holds (by its text, or by its header line when it was edited) is
//! never touched. No model call.

use super::changes::{is_changes_separator, newest_changes_file};
use super::*;
use crate::patch_check::refresh_bullet;
use crate::request_review::changes_checks::{
    has_forbidden_text, is_dependency_bullet, looks_like_version, mismatch, update_target,
};
use std::collections::BTreeSet;

/// One bullet of an entry body: the lines as written, the first without its
/// `- `.
struct Bullet {
    lines: Vec<String>,
}

impl Bullet {
    fn head(&self) -> &str {
        self.lines[0].trim()
    }

    /// The bullet's text, its continuation lines joined on it.
    fn text(&self) -> String {
        let mut text = self.head().to_string();
        for l in &self.lines[1..] {
            text.push(' ');
            text.push_str(l.trim());
        }
        text
    }

    /// The patch a `Refresh <patch>` bullet names, `None` for any other: a
    /// single name that is a patch file (`.patch` or `.diff`).
    fn refreshed(&self) -> Option<&str> {
        let name = self.head().strip_prefix("Refresh ")?.trim();
        let patch = name.ends_with(".patch") || name.ends_with(".diff");
        (self.lines.len() == 1 && patch && !name.contains(char::is_whitespace)).then_some(name)
    }

    /// The patches an already grouped `Refresh patches:` bullet lists.
    fn grouped(&self) -> Option<Vec<&str>> {
        (self.head() == "Refresh patches:").then(|| {
            self.lines[1..]
                .iter()
                .filter_map(|l| l.trim().strip_prefix("* "))
                .map(str::trim)
                .collect()
        })
    }
}

/// The entries of a `.changes` text: the text before the first separator
/// (kept as it is), then each entry from its separator line to the next.
fn split_entries(text: &str) -> (Vec<&str>, Vec<Vec<&str>>) {
    let mut head = Vec::new();
    let mut entries: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if is_changes_separator(line) {
            entries.push(vec![line]);
        } else if let Some(entry) = entries.last_mut() {
            entry.push(line);
        } else {
            head.push(line);
        }
    }
    (head, entries)
}

/// The version a bullet line `- Update to <version>` names, when it names a
/// version (`Update to git snapshot ...` names none).
fn update_version(line: &str) -> Option<String> {
    let target = update_target(line.strip_prefix("- ")?.trim())?;
    looks_like_version(&target).then_some(target)
}

/// The versions the `Update to` bullets of an entry name.
fn update_versions(entry: &[&str]) -> Vec<String> {
    entry.iter().filter_map(|l| update_version(l)).collect()
}

/// `branch` (the text of the package's `.changes`) with its unpublished entries
/// normalised against `origin` (the text of the origin's `.changes`) and the
/// spec's `version`; `package` is the name convention 40 keeps out of bullets.
/// `None` when nothing changes. Lines end as the branch's do (CRLF or LF).
pub fn normalise_unpublished_entries(
    origin: &str,
    branch: &str,
    version: &str,
    package: &str,
) -> Option<String> {
    let origin_entries = split_entries(origin).1;
    let published: BTreeSet<String> = origin_entries
        .iter()
        .map(|e| e.join("\n").trim().to_string())
        .collect();
    // An origin entry edited in the branch is still the origin's: its header
    // line (date and author) tells it apart from this tool's own entries.
    let published_headers: BTreeSet<String> = origin_entries
        .iter()
        .filter_map(|e| e.get(1))
        .map(|h| h.trim().to_string())
        .collect();
    // The versions already published, then those kept from newer entries: an
    // older unpublished entry for one of them is a duplicate.
    let mut covered: Vec<String> = origin_entries
        .iter()
        .flat_map(|e| update_versions(e))
        .collect();
    let (head, entries) = split_entries(branch);
    let mut out: Vec<String> = head.iter().map(|l| l.to_string()).collect();
    for entry in entries {
        let header = entry.get(1).map_or("", |h| h.trim());
        if published.contains(entry.join("\n").trim()) || published_headers.contains(header) {
            out.extend(entry.iter().map(|l| l.to_string()));
            continue;
        }
        let versions = update_versions(&entry);
        // Not an update entry: kept as it is.
        if !entry
            .iter()
            .any(|l| l.starts_with("- ") && update_target(&l[2..]).is_some())
        {
            out.extend(entry.iter().map(|l| l.to_string()));
            continue;
        }
        let duplicate = !versions.is_empty()
            && versions
                .iter()
                .all(|v| covered.iter().any(|c| mismatch(v, c).is_none()));
        if duplicate {
            continue;
        }
        if let Some(lines) = normalised_entry(&entry, version, package) {
            covered.extend(versions);
            out.extend(lines);
        }
    }
    let mut text = format!("{}\n", out.join("\n").trim_end());
    if branch.contains("\r\n") {
        text = text.replace('\n', "\r\n");
    }
    (text.trim_end() != branch.trim_end()).then_some(text)
}

/// The lines of an unpublished update entry, normalised; `None` when the entry
/// goes: it is for another release than `version`, or nothing is left of it.
fn normalised_entry(entry: &[&str], version: &str, package: &str) -> Option<Vec<String>> {
    // The separator, the header line, and the blank lines before the bullets.
    let body_from = entry
        .iter()
        .skip(2)
        .position(|l| l.starts_with("- "))
        .map_or(entry.len(), |i| i + 2);
    let mut bullets: Vec<Bullet> = Vec::new();
    for line in &entry[body_from..] {
        match line.strip_prefix("- ") {
            Some(rest) => bullets.push(Bullet {
                lines: vec![rest.to_string()],
            }),
            None => {
                if let Some(b) = bullets.last_mut() {
                    b.lines.push(line.to_string());
                }
            }
        }
    }
    for b in &mut bullets {
        while b.lines.len() > 1 && b.lines.last().is_some_and(|l| l.trim().is_empty()) {
            b.lines.pop();
        }
    }
    let stale = bullets
        .iter()
        .filter_map(|b| update_target(b.head()))
        .any(|t| looks_like_version(&t) && mismatch(&t, version) == Some(true));
    if stale {
        return None;
    }
    let mut kept: Vec<Bullet> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut patches: Vec<String> = Vec::new();
    let mut group_at: Option<usize> = None;
    for b in bullets {
        let is_update = update_target(b.head()).is_some();
        let text = b.text();
        if !is_update && (is_dependency_bullet(&text) || has_forbidden_text(&text, package)) {
            continue;
        }
        let named: Vec<&str> = match (b.refreshed(), b.grouped()) {
            (Some(one), _) => vec![one],
            (_, Some(many)) => many,
            _ => Vec::new(),
        };
        if !named.is_empty() {
            group_at.get_or_insert(kept.len());
            for n in named {
                if !patches.iter().any(|p| p == n) {
                    patches.push(n.to_string());
                }
            }
            kept.push(Bullet { lines: Vec::new() });
        } else if seen.insert(text) {
            kept.push(b);
        }
    }
    let names: Vec<&str> = patches.iter().map(String::as_str).collect();
    let mut out: Vec<String> = entry[..body_from].iter().map(|l| l.to_string()).collect();
    let mut any = false;
    for (i, b) in kept.iter().enumerate() {
        if b.lines.is_empty() {
            if Some(i) == group_at {
                let text = refresh_bullet(&names);
                let mut lines = text.lines().map(str::to_string);
                if let Some(first) = lines.next() {
                    out.push(format!("- {first}"));
                    out.extend(lines);
                    any = true;
                }
            }
        } else {
            out.push(format!("- {}", b.lines[0]));
            out.extend(b.lines[1..].iter().cloned());
            any = true;
        }
    }
    if !any {
        return None;
    }
    // An entry that needed nothing keeps its own spacing.
    if out.join("\n").trim_end() == entry.join("\n").trim_end() {
        return Some(entry.iter().map(|l| l.to_string()).collect());
    }
    out.push(String::new());
    Some(out)
}

/// [`normalise_unpublished_entries`] on the `.changes` file of the branch
/// package `project/pkg` checked out in `checkout_dir`, against the `.changes`
/// of the package it links to and the Version of the checkout's spec. Whether
/// the file changed is returned. Best effort: a package that is not a link, an
/// origin whose `.changes` cannot be read (every entry would look unpublished)
/// or a file that cannot be read or written is left as it is.
pub fn normalise_unpublished_changes(
    checkout_dir: &Path,
    project: &str,
    pkg: &str,
    emit: Emit<'_>,
) -> bool {
    let Some(file) = newest_changes_file(checkout_dir) else {
        return false;
    };
    let Ok(Some(origin)) = link_origin(project, pkg, emit) else {
        return false;
    };
    let Some(origin_text) = cat_file(
        &origin.project,
        &origin.package,
        &format!("{}.changes", origin.package),
        emit,
    ) else {
        return false;
    };
    let version = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spec"))
        .find_map(|p| fs::read_to_string(p).ok())
        .and_then(|t| crate::spec::parse_header(&t))
        .map(|h| h.version);
    normalise_file(&file, &origin_text, version.as_deref(), pkg)
}

/// [`normalise_unpublished_entries`] on the file `file`, written back when it
/// changes; whether it did. `version` is the spec's Version, `None` when it
/// cannot be read. A Version with a macro (`%{upstream}`: the spec header keeps
/// macros as written) cannot be compared with the bullets, so nothing changes.
pub(super) fn normalise_file(
    file: &Path,
    origin_text: &str,
    version: Option<&str>,
    pkg: &str,
) -> bool {
    let Some(version) = version.filter(|v| !v.contains('%')) else {
        return false;
    };
    let Ok(working) = fs::read_to_string(file) else {
        return false;
    };
    match normalise_unpublished_entries(origin_text, &working, version, pkg) {
        Some(text) => fs::write(file, text).is_ok(),
        None => false,
    }
}
