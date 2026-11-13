//! The upstream recap of an unpublished `Update to <version>` entry that has
//! none under its bullet, filled in by `lint`. The notes are the ones the
//! update stage writes (see [`gather`]), one bullet per release when the jump
//! crosses several, so a package gets the same recap whichever tool wrote it.
//! A published entry, an entry whose bullet already has notes, and a recap
//! nothing was found for are left as they are.

use std::path::Path;

use super::{Recap, archive_in, gather};
use crate::osc::{
    flag_line_mask, is_placeholder, is_release_bullet, newest_changes_file, set_release_notes,
};

/// What a filled entry is reported as, before its versions (`1.7.5..1.9.0`).
pub const FILLED: &str = "filled in the upstream recap";

/// What [`fill_missing_recap`] did to the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fill {
    /// Not an entry to fill: it is published, it is not an update to the spec's
    /// version, or its bullet already has notes.
    Nothing,
    /// The recap was written, from the origin's `old` version to `new`.
    Written { old: String, new: String },
    /// No notes were found: the entry stays as it is. `problem` says why the
    /// releases could not be read, when that is the reason.
    NotFound {
        old: String,
        new: String,
        problem: Option<String>,
    },
}

/// Fills in the missing recap of the newest entry of `pkg`, checked out in
/// `checkout`, against the origin's `.changes` and spec texts. `no_agent` is
/// the run's `--no-agent`: no model condenses the notes.
pub fn fill_missing_recap(
    checkout: &Path,
    pkg: &str,
    origin_changes: &str,
    origin_spec: &str,
    no_agent: bool,
) -> Result<Fill, String> {
    fill_with(
        checkout,
        pkg,
        origin_changes,
        origin_spec,
        no_agent,
        &|repo, archive, old, new, no_agent| gather(repo, archive, Some(old), new, no_agent),
    )
}

/// [`fill_missing_recap`] with the recap given by `recap` (`repo_url`, the
/// archive, the old and the new version, `no_agent`), so the choice of the
/// entry and the writing are tested without a network.
pub(super) fn fill_with(
    checkout: &Path,
    pkg: &str,
    origin_changes: &str,
    origin_spec: &str,
    no_agent: bool,
    recap: &dyn Fn(Option<&str>, Option<&Path>, &str, &str, bool) -> Recap,
) -> Result<Fill, String> {
    let Some(old) = crate::spec::parse_header(origin_spec).map(|h| h.version) else {
        return Ok(Fill::Nothing);
    };
    let Ok(spec) = std::fs::read_to_string(checkout.join(format!("{pkg}.spec"))) else {
        return Ok(Fill::Nothing);
    };
    let Some(new) = crate::spec::parse_header(&spec).map(|h| h.version) else {
        return Ok(Fill::Nothing);
    };
    let Some(file) = newest_changes_file(checkout) else {
        return Ok(Fill::Nothing);
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    if old == new || !needs_recap(&text, origin_changes, &new) {
        return Ok(Fill::Nothing);
    }
    let repo = crate::bugfix::upstream_repo_of_spec(&spec);
    let archive = archive_in(checkout, &new);
    let found = recap(repo.as_deref(), archive.as_deref(), &old, &new, no_agent);
    let nothing = found.releases.iter().all(|(_, lines)| lines.is_empty());
    // The first release is the update's own: the bullet the notes go under.
    if nothing || found.releases.first().is_none_or(|(v, _)| *v != new) {
        return Ok(Fill::NotFound {
            old,
            new,
            problem: found.fetch_problem,
        });
    }
    set_release_notes(checkout, &found.releases).map_err(|e| e.to_string())?;
    Ok(Fill::Written { old, new })
}

/// Whether the newest entry of `text` is unpublished (the origin's `.changes`
/// has neither its text nor its header), has the `Update to <new>` bullet, and
/// has no notes under it: a flag line and a placeholder are not notes.
fn needs_recap(text: &str, origin_changes: &str, new: &str) -> bool {
    let Some(entry) = entries_of(text).into_iter().next() else {
        return false;
    };
    if published(&entry, origin_changes) {
        return false;
    }
    let Some(at) = entry.iter().position(|l| {
        !l.starts_with(' ') && is_release_bullet(l.trim().trim_start_matches(['-', '*', ' ']), new)
    }) else {
        return false;
    };
    let rest = &entry[at + 1..];
    let block_end = rest
        .iter()
        .position(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .unwrap_or(rest.len());
    let block = &rest[..block_end];
    let flags = flag_line_mask(block);
    !block
        .iter()
        .enumerate()
        .any(|(i, l)| l.starts_with(' ') && !l.trim().is_empty() && !flags[i] && !is_placeholder(l))
}

/// Whether `entry` is an entry of `origin_changes`: the same text, or the same
/// header (its date and author).
fn published(entry: &[&str], origin_changes: &str) -> bool {
    let text = entry.join("\n");
    let text = text.trim();
    let header = header_of(entry);
    entries_of(origin_changes)
        .iter()
        .any(|e| e.join("\n").trim() == text || (!header.is_empty() && header_of(e) == header))
}

/// The header of an entry: its first non-blank line.
fn header_of<'a>(entry: &[&'a str]) -> &'a str {
    entry
        .iter()
        .find(|l| !l.trim().is_empty())
        .map_or("", |l| l.trim())
}

/// The entries of a `.changes` text, each without its separator line, in
/// file order (the newest first).
fn entries_of(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        if is_separator(line) {
            out.push(Vec::new());
        } else if let Some(entry) = out.last_mut() {
            entry.push(line);
        }
    }
    out
}

fn is_separator(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 20 && line.bytes().all(|b| b == b'-')
}
