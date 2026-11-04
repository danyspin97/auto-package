//! A fix round whose only spec edit prints diagnostics (an `echo` or a `cat` of
//! a test log in `%build`, `%check` or `%install`) runs a whole build without
//! changing what the build does. After one such round the next turn is told so,
//! and a second one in a row is rolled back: the agent must change what can fix
//! the failure, or answer `no_change` or `blocked`.

use super::snapshot::CheckoutSnapshot;
use std::fs;
use std::path::Path;

/// The sections whose lines run while the package is built or tested.
const BUILD_SECTIONS: &[&str] = &["%build", "%check", "%install"];

/// The first words of a line that only prints.
const PRINT_WORDS: &[&str] = &[
    "echo", "printf", "cat", "tail", "head", "ls", "find", "pwd", "uname",
];

/// Tokens that make a line do more than print: a redirect, a pipe, a chain, a
/// substitution, or a `find` action that writes or deletes.
const EFFECTS: &[&str] = &[
    ">", "|", "&", ";", "`", "$(", "-delete", "-exec", "-ok", "-fprint", "-fls",
];

/// Whether `line` only prints: it starts with a print word, names no `VAR=`
/// setting, and has no redirect, pipe, chain, substitution or file action. `env`
/// prints only when given no arguments; `set` only as `set -x` or `set +x`.
fn prints_only(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    if lower == "set -x" || lower == "set +x" || lower == "env" {
        return true;
    }
    let mut words = lower.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if !PRINT_WORDS.contains(&first) || first.contains('=') {
        return false;
    }
    !lower
        .split_whitespace()
        .any(|w| w.contains('=') && !w.starts_with('-'))
        && !EFFECTS.iter().any(|e| lower.contains(e))
}

/// Whether a spec line of `section` is a build-time print: it needs no
/// `.changes` bullet (see `changelog_required`).
pub(crate) fn is_build_print(section: &str, line: &str) -> bool {
    BUILD_SECTIONS
        .iter()
        .any(|s| section.split_whitespace().next() == Some(s))
        && prints_only(line)
}

/// Whether the edit from `before` to `after` changes something, and every line
/// it changes is a build-time print.
pub(crate) fn diagnostic_only(before: &str, after: &str) -> bool {
    let mut counts: std::collections::BTreeMap<(String, String, String), i64> = Default::default();
    for k in super::changelog_required::keyed_lines(before) {
        *counts.entry(k).or_default() -= 1;
    }
    for k in super::changelog_required::keyed_lines(after) {
        *counts.entry(k).or_default() += 1;
    }
    let changed: Vec<(String, String)> = counts
        .into_iter()
        .filter(|(_, n)| *n != 0)
        .map(|((section, _, line), _)| (section, line))
        .collect();
    !changed.is_empty()
        && changed
            .iter()
            .all(|(section, line)| is_build_print(section, line))
}

/// The marker a diagnostic round leaves in the package workspace: the run that
/// wrote it. A marker of another run is no marker.
const MARKER: &str = "diagnostic-round";

/// Whether the last fix round of `run_id` only printed diagnostics.
pub(super) fn last_round_diagnostic(ws_dir: &Path, run_id: &str) -> bool {
    fs::read_to_string(ws_dir.join(MARKER)).is_ok_and(|s| s.trim() == run_id)
}

/// Record (or clear) that the last fix round of `run_id` only printed.
pub(super) fn set_last_round_diagnostic(ws_dir: &Path, run_id: &str, value: bool) {
    if value {
        let _ = fs::write(ws_dir.join(MARKER), run_id);
    } else {
        let _ = fs::remove_file(ws_dir.join(MARKER));
    }
}

/// What the next fix turn is told after a diagnostic-only round.
pub(super) fn diagnostic_round_note() -> &'static str {
    "The previous fix round changed only diagnostic output (print or `cat` lines in \
     %build, %check or %install): its build ran the same package and failed the same way. \
     This round must change what can fix the failure: a build flag, a patch, a dependency, \
     the line that fails. A second diagnostic-only edit in a row is rolled back. If nothing \
     in the spec can fix it, answer no_change or blocked with the reason."
}

/// Whether a top-level file of `checkout_dir` other than `spec_name` is new or
/// differs from the snapshot taken before the turn. A snapshotted file whose
/// bytes were not kept is not judged.
pub(crate) fn other_files_changed(
    checkout_dir: &Path,
    snapshot: &CheckoutSnapshot,
    spec_name: &str,
) -> bool {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return false;
    };
    rd.flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != spec_name && !n.starts_with('.'))
        .any(|name| match snapshot.entry_of(&name) {
            None => true,
            Some(None) => false,
            Some(Some(old)) => fs::read(checkout_dir.join(&name)).is_ok_and(|now| now != old),
        })
}

/// What a fix round does to the marker and the turn (see `fix_after_build`).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RoundVerdict {
    /// Roll the turn back: a second diagnostic-only round in a row.
    Refuse,
    /// The round only printed: the next round is told so.
    Mark,
    /// Any other round (a real change, another file, or no change) clears it.
    Clear,
}

/// The verdict of a round: diagnostic only when the spec changed and only prints,
/// no other file changed, and the round is not the second in a row.
pub(super) fn round_verdict(
    spec_changed: bool,
    diagnostic_spec: bool,
    other_changed: bool,
    marked_before: bool,
) -> RoundVerdict {
    if !(spec_changed && diagnostic_spec) || other_changed {
        RoundVerdict::Clear
    } else if marked_before {
        RoundVerdict::Refuse
    } else {
        RoundVerdict::Mark
    }
}

#[cfg(test)]
#[path = "diagnostic_edit_tests.rs"]
mod diagnostic_edit_tests;
