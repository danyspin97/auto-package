//! The unpublished entries of a branch package's `.changes`, normalised. An
//! entry the origin's `.changes` does not hold is this tool's own work, not
//! published history, and may be rewritten. An update entry (one with an
//! `Update to <version>` bullet) goes when its version is another release than
//! the spec's, or when the origin already publishes that version. The exception
//! is an older release than the spec's: its entry is kept, as an entry of its
//! own (a submit carries every unpublished update up to the spec's version); a
//! newer one, or one that cannot be ordered against the spec's, goes. The newest
//! entry of the file is kept, with a warning, when only its version is another
//! release. The update entries of one version are merged into one, under the
//! newest one's header line: the flags and recap lines of its update bullet
//! come from the newest entry alone, and the other bullets of all of them
//! follow, oldest first. The bullets convention 40 forbids, and the
//! placeholders that say no notes were found, are dropped, exact duplicates
//! are joined and `Refresh <x>.patch` bullets are grouped. Any other
//! unpublished entry is kept as it is. An entry the origin holds (by its text,
//! or by its header line when it was edited) is never touched. No model call.

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

    /// The continuation lines of a grouped `Refresh patches:` bullet that are
    /// not `* ` items, kept after the group as they are written.
    fn group_notes(&self) -> Vec<String> {
        if self.head() != "Refresh patches:" {
            return Vec::new();
        }
        self.lines[1..]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("* "))
            .map(|l| format!("  {l}"))
            .collect()
    }
}

/// The entries of a `.changes` text: the text before the first separator
/// (kept as it is), then each entry from its separator line to the next.
pub(super) fn split_entries(text: &str) -> (Vec<&str>, Vec<Vec<&str>>) {
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

/// The texts a recap writes when no upstream notes were found, in place of
/// notes. They are not changes and never belong in a `.changes` entry.
const PLACEHOLDERS: [&str; 4] = [
    "no upstream changelog",
    "no upstream changelog found or provided",
    "no upstream release notes",
    "no release notes",
];

/// Whether `line` is only such a placeholder, as a bullet or as a nested
/// bullet (`  * No upstream changelog found or provided`).
pub fn is_placeholder(line: &str) -> bool {
    let text = line
        .trim()
        .trim_start_matches(['-', '*'])
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    PLACEHOLDERS.contains(&text.as_str())
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

/// The text an `Update to` bullet head says after its version (`* a` for
/// `Update to 5.4 * a`), `None` when it says nothing more.
fn update_rest(head: &str) -> Option<String> {
    let rest = head.strip_prefix("Update to ")?.trim_start();
    let rest = rest.strip_prefix("version ").unwrap_or(rest).trim_start();
    let rest = rest.split_once(char::is_whitespace).map_or("", |(_, r)| r);
    let rest = rest.trim_start_matches(':').trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Whether an `Update to` bullet names another release than `version`.
fn is_foreign_update(bullet: &Bullet, version: &str) -> bool {
    update_target(bullet.head())
        .is_some_and(|t| looks_like_version(&t) && mismatch(&t, version) == Some(true))
}

/// Whether `release` is an older release than `version`, in the generic order
/// (`Scheme::Generic`: the spec's scheme is not known here). `false` when the
/// two cannot be ordered, so such an entry is dropped as before.
fn is_older(release: &str, version: &str) -> bool {
    crate::version::cmp(release, version, crate::version::Scheme::Generic)
        == Some(std::cmp::Ordering::Less)
}

/// What a normalisation did beyond the text, for the run's warnings.
#[derive(Debug, Default, PartialEq)]
pub struct Notes {
    /// Placeholder bullets were removed from unpublished entries.
    pub placeholder: bool,
    /// Unpublished entries of one version were merged.
    pub merged: bool,
    /// The newest entry is kept although it names this other version.
    pub stale_newest: Option<String>,
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
    normalise_with_notes(origin, branch, version, package).0
}

/// [`normalise_unpublished_entries`] with what it did beyond the text.
pub(super) fn normalise_with_notes(
    origin: &str,
    branch: &str,
    version: &str,
    package: &str,
) -> (Option<String>, Notes) {
    let mut notes = Notes::default();
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
    // The versions the origin publishes: an unpublished entry for one of them
    // is a duplicate of it.
    let published_versions: Vec<String> = origin_entries
        .iter()
        .flat_map(|e| update_versions(e))
        .collect();
    let (head, entries) = split_entries(branch);
    // What becomes of each entry: kept as it is, dropped, or a member of a
    // group of unpublished update entries for one version.
    let mut fates: Vec<Fate> = Vec::new();
    // Per group: its members, newest first (indexes into `entries`), and the
    // version of its newest member.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_versions: Vec<Option<String>> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let header = entry.get(1).map_or("", |h| h.trim());
        if published.contains(entry.join("\n").trim())
            || published_headers.contains(header)
            || !is_update_entry(entry)
        {
            fates.push(Fate::Keep);
            continue;
        }
        let versions = update_versions(entry);
        let duplicate = !versions.is_empty()
            && versions
                .iter()
                .all(|v| published_versions.iter().any(|c| mismatch(v, c).is_none()));
        if duplicate {
            fates.push(Fate::Drop);
            continue;
        }
        // Staleness is judged per entry, on its first `Update to` version. The
        // newest entry of the file is never dropped for it: it is kept, and
        // reported. An older release than the spec's is kept too, grouped
        // below as the release it is.
        let primary = versions.first().cloned();
        if let Some(p) = primary
            .as_deref()
            .filter(|p| mismatch(p, version) == Some(true))
        {
            if i == 0 {
                notes.stale_newest = Some(p.to_string());
                fates.push(Fate::Keep);
                continue;
            }
            if !is_older(p, version) {
                fates.push(Fate::Drop);
                continue;
            }
        }
        let same_release = |g: &Option<String>| match (g, &primary) {
            (Some(g), Some(p)) => mismatch(p, g).is_none(),
            _ => false,
        };
        match group_versions.iter().position(same_release) {
            Some(g) => {
                groups[g].push(i);
                fates.push(Fate::Member(g));
            }
            None => {
                groups.push(vec![i]);
                group_versions.push(primary);
                fates.push(Fate::Member(groups.len() - 1));
            }
        }
    }
    notes.merged = groups.iter().any(|m| m.len() > 1);
    notes.placeholder = entries
        .iter()
        .zip(&fates)
        .any(|(e, f)| matches!(f, Fate::Member(_)) && e.iter().any(|l| is_placeholder(l)));
    let merged: Vec<Option<Vec<String>>> = groups
        .iter()
        .zip(&group_versions)
        .map(|(members, own)| {
            merged_entry(
                &entries,
                members,
                own.as_deref().unwrap_or(version),
                package,
            )
        })
        .collect();
    let mut out: Vec<String> = head.iter().map(|l| l.to_string()).collect();
    for (i, (entry, fate)) in entries.iter().zip(&fates).enumerate() {
        match fate {
            Fate::Keep => out.extend(entry.iter().map(|l| l.to_string())),
            Fate::Drop => {}
            // A group is written once, where its newest member stood.
            Fate::Member(g) => {
                if let Some(lines) = &merged[*g]
                    && groups[*g][0] == i
                {
                    out.extend(lines.iter().cloned());
                }
            }
        }
    }
    let body = out.join("\n");
    let body = body.trim_end();
    // The branch's own trailing blank lines stay when the rewrite does not
    // reach the end of the file; otherwise the file ends in one newline.
    let branch_end = branch.trim_end();
    let tail = if branch_end.lines().last() == body.lines().last() {
        branch[branch_end.len()..].replace("\r\n", "\n")
    } else {
        "\n".to_string()
    };
    let mut text = format!("{body}{tail}");
    if branch.contains("\r\n") {
        text = text.replace('\n', "\r\n");
    }
    let changed = (text.trim_end() != branch.trim_end()).then_some(text);
    (changed, notes)
}

/// What [`normalise_unpublished_entries`] does with one entry of the branch.
enum Fate {
    Keep,
    Drop,
    /// A member of the group with this index.
    Member(usize),
}

/// Whether an entry has an `- Update to <version>` bullet, or any other
/// `Update to` bullet.
fn is_update_entry(entry: &[&str]) -> bool {
    entry
        .iter()
        .any(|l| l.starts_with("- ") && update_target(&l[2..]).is_some())
}

/// The index of the first bullet of an entry: the separator, the header line
/// and the blank lines before the bullets come before it. The entry's length
/// when it has no bullet.
fn body_start(entry: &[&str]) -> usize {
    entry
        .iter()
        .skip(2)
        .position(|l| l.starts_with("- "))
        .map_or(entry.len(), |i| i + 2)
}

/// The lines of an entry's body (from its first bullet) that belong to an
/// `Update to` bullet, its continuation lines included, and the others.
fn split_blocks(entry: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut update = Vec::new();
    let mut other = Vec::new();
    let mut in_update = false;
    for line in &entry[body_start(entry)..] {
        if let Some(rest) = line.strip_prefix("- ") {
            in_update = update_target(rest.trim()).is_some();
        }
        if in_update {
            update.push(line.to_string());
        } else {
            other.push(line.to_string());
        }
    }
    (update, other)
}

/// The lines of a group of unpublished update entries for one version, as one
/// entry. `members` are indexes into `entries`, newest first; `version` is the
/// group's own release (the spec's Version for a group of it). The header of
/// the newest member comes first, then its update bullet, then the other
/// bullets of all the members, oldest first. The flags and recap lines of an
/// update bullet are the newest entry's alone: the rewrites of this run touch
/// only that entry, so an older entry's copies are stale. Older entries keep
/// their other bullets, a `Refresh` one included: no rewrite of this run
/// reaches it, and a bullet that is still true stays grouped with the rest.
/// Text before an older entry's first bullet is dropped. `None` as
/// [`normalised_entry`] gives it.
fn merged_entry(
    entries: &[Vec<&str>],
    members: &[usize],
    version: &str,
    package: &str,
) -> Option<Vec<String>> {
    let newest = &entries[members[0]];
    let body = body_start(newest);
    let mut lines: Vec<String> = newest[..body].iter().map(|l| l.to_string()).collect();
    if members.len() == 1 {
        lines.extend(newest[body..].iter().map(|l| l.to_string()));
    } else {
        lines.extend(split_blocks(newest).0);
        for &m in members.iter().rev() {
            lines.extend(split_blocks(&entries[m]).1);
        }
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    normalised_entry(&refs, version, package)
}

/// The lines of an unpublished update entry, normalised; `None` when nothing
/// is left of it. `version` is the release the entry is for: an `Update to`
/// bullet for another one goes.
fn normalised_entry(entry: &[&str], version: &str, package: &str) -> Option<Vec<String>> {
    let body_from = body_start(entry);
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
    bullets.retain(|b| !is_placeholder(b.head()));
    for b in &mut bullets {
        b.lines.retain(|l| !is_placeholder(l));
        while b.lines.len() > 1 && b.lines.last().is_some_and(|l| l.trim().is_empty()) {
            b.lines.pop();
        }
    }
    // An `Update to` bullet for another release than the entry's own is no
    // part of this entry (a bullet with its lines goes with it).
    bullets.retain(|b| !is_foreign_update(b, version));
    let mut kept: Vec<Bullet> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // The `Update to` bullets kept, with the version each names: a repeat of
    // one adds the recap lines and the text the kept one lacks.
    let mut updates: Vec<(String, usize)> = Vec::new();
    let mut patches: Vec<String> = Vec::new();
    let mut group_notes: Vec<String> = Vec::new();
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
            group_notes.extend(b.group_notes());
            kept.push(Bullet { lines: Vec::new() });
        } else if is_update {
            let target = update_target(b.head()).unwrap_or_default();
            let same = updates
                .iter()
                .find(|(t, _)| mismatch(t, &target).is_none())
                .map(|&(_, at)| at);
            match same {
                Some(at) => {
                    let mut extra: Vec<String> = Vec::new();
                    extra.extend(update_rest(b.head()).map(|r| format!("  {r}")));
                    extra.extend(b.lines[1..].iter().cloned());
                    for l in extra {
                        if !kept[at].lines.contains(&l) {
                            kept[at].lines.push(l);
                        }
                    }
                }
                None => {
                    updates.push((target, kept.len()));
                    kept.push(b);
                }
            }
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
                    out.extend(group_notes.iter().cloned());
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
/// of the package it links to and the Version of the checkout's spec. The
/// messages of the run are returned, empty when the file is left as it is.
/// Best effort: a package that is not a link, an origin whose `.changes`
/// cannot be read (every entry would look unpublished) or a file that cannot
/// be read or written is left as it is.
pub fn normalise_unpublished_changes(
    checkout_dir: &Path,
    project: &str,
    pkg: &str,
    emit: Emit<'_>,
) -> Vec<String> {
    let Ok(Some(origin)) = link_origin(project, pkg, emit) else {
        return Vec::new();
    };
    let Some(origin_text) = cat_file(
        &origin.project,
        &origin.package,
        &format!("{}.changes", origin.package),
        emit,
    ) else {
        return Vec::new();
    };
    normalise_checkout(checkout_dir, &origin_text, pkg)
}

/// [`normalise_unpublished_changes`] with the origin's `.changes` text given:
/// the newest `.changes` of `checkout_dir`, normalised against it and the
/// checkout's spec Version.
pub(super) fn normalise_checkout(checkout_dir: &Path, origin_text: &str, pkg: &str) -> Vec<String> {
    let Some(file) = newest_changes_file(checkout_dir) else {
        return Vec::new();
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
    normalise_file(&file, origin_text, version.as_deref(), pkg)
}

/// [`normalise_unpublished_entries`] on the file `file`, written back when it
/// changes. Returns the messages of the run: empty when the file is left as
/// it is. `version` is the spec's Version, `None` when it cannot be read. A
/// Version with a macro (`%{upstream}`: the spec header keeps macros as
/// written) cannot be compared with the bullets, so nothing changes.
pub(super) fn normalise_file(
    file: &Path,
    origin_text: &str,
    version: Option<&str>,
    pkg: &str,
) -> Vec<String> {
    let Some(version) = version.filter(|v| !v.contains('%')) else {
        return Vec::new();
    };
    let Ok(working) = fs::read_to_string(file) else {
        return Vec::new();
    };
    let (text, notes) = normalise_with_notes(origin_text, &working, version, pkg);
    if let Some(text) = &text
        && fs::write(file, text).is_err()
    {
        return Vec::new();
    }
    let mut messages = Vec::new();
    if notes.placeholder {
        messages.push("removed the placeholder bullets of its unpublished entries".to_string());
    }
    if notes.merged {
        messages.push("merged the unpublished entries of one version".to_string());
    }
    if let Some(stale) = &notes.stale_newest {
        messages.push(format!(
            "kept the newest unpublished entry, for {stale}: the spec's Version is {version}, check it"
        ));
    }
    if text.is_some() && messages.is_empty() {
        messages.push("normalised the unpublished entries of its .changes".to_string());
    }
    messages
}
