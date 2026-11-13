//! The leftovers of a branch package's checkout, cleaned in one place by the
//! commit step and by the tidy commit: the unpublished `.changes` entries
//! normalised against the origin (dependency and placeholder bullets among
//! them), the FLAG lines of those entries that the origin's spec already
//! explains, and the directories a service run left at the top of the
//! checkout. Stray files, old archives and the quilt `series` are not here:
//! the commit removes them after its publish plan, and refuses some plans
//! before it does.

use super::changes_flag_lines::{flag_line_mask, is_flag_start};
use super::changes_normalise::{normalise_checkout, split_entries};
use super::service_dirs::remove_service_source_dirs;
use super::*;
use crate::fix_flags::{FixFlag, from_spec_diff};
use crate::release_notes::{FILLED, Fill, fill_missing_recap};
use ap_core::event::Event;
use std::collections::BTreeSet;

/// Cleans the leftovers of the branch package `pkg` of `project`, checked out
/// in `checkout`. `named` is what the expanded spec names. Returns one message
/// per change made, empty when nothing was left to clean. Best effort: an
/// origin that cannot be read leaves the `.changes` and the flags as they are.
/// A missing upstream recap of the update is filled in (`no_agent`: as the run
/// was asked, no model condenses it); what is not found is reported on `emit`.
pub fn clean_branch_leftovers(
    checkout: &Path,
    project: &str,
    pkg: &str,
    named: &[String],
    no_agent: bool,
    emit: Emit<'_>,
) -> Vec<String> {
    let origin = match link_origin(project, pkg, emit) {
        Ok(Some(origin)) => Some(origin),
        _ => None,
    };
    let read = |ext: &str| {
        origin.as_ref().and_then(|o| {
            cat_file(
                &o.project,
                &o.package,
                &format!("{}.{ext}", o.package),
                emit,
            )
        })
    };
    let origin_changes = read("changes");
    let origin_spec = read("spec");
    let warn = |message: String| {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message,
        })
    };
    clean_with_origin(
        checkout,
        origin_changes.as_deref(),
        origin_spec.as_deref(),
        pkg,
        named,
        no_agent,
        &warn,
    )
}

/// Whether [`clean_branch_leftovers`] would drop a FLAG line of `pkg`'s
/// unpublished entries in `project` that the origin `origin_project/origin_pkg`
/// already explains. Read only: an unreadable text counts as not stale.
pub fn has_explained_flags(
    project: &str,
    pkg: &str,
    origin_project: &str,
    origin_pkg: &str,
    emit: Emit<'_>,
) -> bool {
    let read = |p: &str, q: &str, ext: &str| cat_file(p, q, &format!("{q}.{ext}"), emit);
    let (Some(changes), Some(spec), Some(origin_changes), Some(origin_spec)) = (
        read(project, pkg, "changes"),
        read(project, pkg, "spec"),
        read(origin_project, origin_pkg, "changes"),
        read(origin_project, origin_pkg, "spec"),
    ) else {
        return false;
    };
    without_explained_flags(&changes, &origin_changes, &origin_spec, &spec).is_some()
}

/// [`clean_branch_leftovers`] with the origin's `.changes` and spec texts
/// given (`None` when the package has no readable origin). `warn` gets what
/// the fill of the recap could not do.
pub(super) fn clean_with_origin(
    checkout: &Path,
    origin_changes: Option<&str>,
    origin_spec: Option<&str>,
    pkg: &str,
    named: &[String],
    no_agent: bool,
    warn: &dyn Fn(String),
) -> Vec<String> {
    let mut messages = origin_changes
        .map(|text| normalise_checkout(checkout, text, pkg))
        .unwrap_or_default();
    if let (Some(changes), Some(spec)) = (origin_changes, origin_spec)
        && drop_explained_flags(checkout, changes, spec, pkg)
    {
        messages.push("dropped the FLAG lines its origin's spec already explains".to_string());
    }
    if let (Some(changes), Some(spec)) = (origin_changes, origin_spec) {
        match fill_missing_recap(checkout, pkg, changes, spec, no_agent) {
            Ok(Fill::Written { old, new }) => messages.push(format!("{FILLED} for {old}..{new}")),
            Ok(Fill::NotFound { old, new, problem }) => warn(format!(
                "no upstream notes available for {old}..{new}, so the {new} entry keeps none{}",
                problem.map(|p| format!(" ({p})")).unwrap_or_default()
            )),
            Ok(Fill::Nothing) => {}
            Err(why) => warn(format!("the upstream recap was not filled in: {why}")),
        }
    }
    for name in remove_service_source_dirs(checkout, named) {
        messages.push(format!(
            "deleted '{name}/' from the checkout: a service left it there"
        ));
    }
    messages
}

/// Rewrites the newest `.changes` of `checkout` with
/// [`without_explained_flags`], against the checkout's `<pkg>.spec`. Whether
/// the file changed.
fn drop_explained_flags(
    checkout: &Path,
    origin_changes: &str,
    origin_spec: &str,
    pkg: &str,
) -> bool {
    let Some(file) = newest_changes_file(checkout) else {
        return false;
    };
    let (Ok(text), Ok(spec)) = (
        fs::read_to_string(&file),
        fs::read_to_string(checkout.join(format!("{pkg}.spec"))),
    ) else {
        return false;
    };
    match without_explained_flags(&text, origin_changes, origin_spec, &spec) {
        Some(new) => fs::write(&file, new).is_ok(),
        None => false,
    }
}

/// `changes` (a branch package's `.changes`) with the spec-derived FLAG lines
/// of its unpublished entries recomputed against the origin: a flag the diff
/// of `origin_spec` to `spec` no longer reports is removed, so only what is
/// still new stays. A tests-skipped flag that is partly explained is replaced
/// by the new part, once. Kept as they are: a published entry (its text or its
/// header is the origin's), and any flag the spec diff does not write (a fix
/// turn's `blocked` answer). `None` when nothing changes.
pub(super) fn without_explained_flags(
    changes: &str,
    origin_changes: &str,
    origin_spec: &str,
    spec: &str,
) -> Option<String> {
    let fresh: Vec<String> = from_spec_diff(Some(origin_spec), spec)
        .iter()
        .map(FixFlag::line)
        .collect();
    let (_, origin_entries) = split_entries(origin_changes);
    let published: BTreeSet<String> = origin_entries
        .iter()
        .map(|e| e.join("\n").trim().to_string())
        .collect();
    let published_headers: BTreeSet<String> = origin_entries
        .iter()
        .filter_map(|e| e.get(1))
        .map(|h| h.trim().to_string())
        .collect();
    let (head, entries) = split_entries(changes);
    let mut out: Vec<String> = head.iter().map(|l| l.to_string()).collect();
    let mut changed = false;
    let mut partly_written = false;
    for entry in &entries {
        let header = entry.get(1).map_or("", |h| h.trim());
        if published.contains(entry.join("\n").trim()) || published_headers.contains(header) {
            out.extend(entry.iter().map(|l| l.to_string()));
            continue;
        }
        let mask = flag_line_mask(entry);
        let mut i = 0;
        while i < entry.len() {
            if !mask[i] {
                out.push(entry[i].to_string());
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < entry.len() && mask[i] && !is_flag_start(entry[i]) {
                i += 1;
            }
            let group = &entry[start..i];
            let text = flat(&group.join(" "));
            if !is_spec_flag(&text) || fresh.contains(&text) {
                out.extend(group.iter().map(|l| l.to_string()));
                continue;
            }
            changed = true;
            let new_tests = fresh.iter().find(|f| f.starts_with(TESTS_SKIPPED));
            if let Some(line) =
                new_tests.filter(|_| text.starts_with(TESTS_SKIPPED) && !partly_written)
            {
                partly_written = true;
                out.push(format!("  {line}"));
            }
        }
    }
    if !changed {
        return None;
    }
    let eol = if changes.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut text = out.join(eol);
    if changes.ends_with('\n') {
        text.push_str(eol);
    }
    Some(text)
}

const TESTS_SKIPPED: &str = "FLAG: tests skipped: ";

/// Whether a flag (its text on one line) is one the spec diff writes: skipped
/// tests, a removed `%check`, a switch to the upstream wheel.
fn is_spec_flag(text: &str) -> bool {
    text.starts_with(TESTS_SKIPPED)
        || text.starts_with("FLAG: %check removed")
        || text.starts_with("FLAG: built from the upstream wheel ")
}

/// `text` with its words single-spaced.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
