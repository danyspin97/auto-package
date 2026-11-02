//! Rejected edits: the spec rejections, the patch escapes and the accept-or-restore
//! rules that gate a turn's edits.

use super::*;
use crate::pipeline::licence::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::source_guard::*;

/// How many times the same validation error rejected an edit of one spec, in
/// this process. A fourth identical turn would be re-prompting for the same
/// failure (python-torch: eleven rejections of one inherited patch, $2.64).
type SpecRejections = std::collections::HashMap<(PathBuf, u64), (u32, String)>;

static SPEC_REJECTIONS: std::sync::LazyLock<std::sync::Mutex<SpecRejections>> =
    std::sync::LazyLock::new(Default::default);

/// Identical rejections of one spec after which the step fails instead of asking again.
const MAX_IDENTICAL_REJECTIONS: u32 = 3;

pub(super) fn note_spec_rejection(spec_path: &Path, problem: &str) {
    let mut map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = map
        .entry((spec_path.to_path_buf(), hash_spec(problem)))
        .or_insert((0, problem.to_string()));
    entry.0 += 1;
}

pub(super) fn clear_spec_rejections(spec_path: &Path) {
    let mut map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|(p, _), _| p != spec_path);
}

/// The failure for a spec rejected with the same error [`MAX_IDENTICAL_REJECTIONS`]
/// times, or `None` while another try is still reasonable.
pub(super) fn repeated_spec_rejection(spec_path: &Path) -> Option<String> {
    let map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    map.iter()
        .find(|((p, _), (n, _))| p == spec_path && *n >= MAX_IDENTICAL_REJECTIONS)
        .map(|(_, (n, problem))| {
            let first: String = problem
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            format!(
                "the spec was rejected {n} times with the same error and the agent does not \
                 get past it: {first}"
            )
        })
}

/// The patch-escape check on its own, for a turn whose result the rest of the
/// acceptance gate does not judge (an unchanged spec, the npm-vendor and the
/// refused-wheel arms). On an escape the spec goes back to `before`, the
/// snapshot (when there is one) is restored keeping `keep`, and the rejection
/// is noted. Returns true when the edit was rolled back.
pub(super) fn reject_patch_escape(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let baseline = patch_baseline(snapshot, spec_path);
    let Some((offender, problem)) = patch_escape_offender(spec_path, &after, &baseline, None)
    else {
        return None;
    };
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::RpmspecParseFailed {
            stderr: problem.clone(),
        },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    // Without a snapshot there is no pre-turn copy of the patch files. A patch
    // the turn created (the spec before the turn did not name it, OBS does not
    // publish it, and it is not a staged file) is deleted, so the rejected
    // patch does not stay behind as an unreferenced file. Any other patch is
    // left as the turn wrote it and the note says so.
    let mut removed = false;
    if snapshot.is_none() {
        let published = match &baseline {
            PatchBaseline::NoBaseline(p) | PatchBaseline::NotPublished(p) => p.as_slice(),
            PatchBaseline::Snapshot(_) => &[],
        };
        let created_by_turn = !before.contains(&offender)
            && !published.contains(&offender)
            && !keep.contains(&offender);
        if created_by_turn {
            removed = std::fs::remove_file(spec_path.with_file_name(&offender)).is_ok();
        }
    }
    let rollback = match (snapshot.is_some(), removed) {
        (true, _) => "The edit was rolled back.",
        (false, true) => {
            "The spec is back at its pre-turn text, and the offending patch, which the \
             turn created, was deleted."
        }
        (false, false) => {
            "The spec is back at its pre-turn text, but the patch files were not restored \
             (no pre-turn copy of the checkout was kept): the offending patch stays as the \
             turn left it and must be fixed."
        }
    };
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — a patch escapes the source tree\n{problem}\n\
             {rollback} Patch headers must name paths relative to the \
             source tree, with no leading `/`, `~`, or `..` component.\n"
        ),
    );
    Some(problem)
}

/// The gate for a turn whose arm answers `NoChange` (the npm-vendor deferral and
/// the refused wheel switch): a spec the turn edited goes through the full
/// acceptance gate, an unchanged spec only through the patch-escape check. A
/// rejection restores the snapshot keeping `keep`. Returns true when the turn's
/// spec edit was accepted.
pub(super) fn gate_turn_edit_without_accept(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_entry: &str,
    notes_path: PathBuf,
    keep: &[String],
) -> bool {
    let spec_edited = std::fs::read_to_string(spec_path).ok().as_deref() != Some(before);
    if spec_edited {
        accept_or_restore_with_snapshot(
            spec_path,
            before,
            snapshot,
            step,
            reporter,
            notes_entry,
            notes_path,
            keep,
        )
    } else {
        reject_patch_escape(
            spec_path, before, snapshot, step, reporter, notes_path, keep,
        )
        .is_none()
    }
}

/// The acceptance gate for an agent edit. A full-checkout snapshot taken
/// BEFORE the agent turn ran is optional: on reject (rpmspec --parse fails on
/// the agent's new spec), the spec AND every other top-level checkout file the
/// agent may have edited are rolled back from it. A `None` snapshot means
/// "spec-only restore", for edits that must keep files written outside the
/// snapshot (the staged files of a turn).
///
/// A rejected turn must roll back the whole checkout, not only the spec: a
/// `.changes`/patch/`_service` edit made in the same turn would otherwise
/// survive the rejection, and the next turn could commit it.
pub(super) fn accept_or_restore_with_snapshot(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_entry: &str,
    notes_path: PathBuf,
    keep: &[String],
) -> bool {
    accept_or_restore_reasoned(
        spec_path,
        before,
        snapshot,
        step,
        reporter,
        notes_entry,
        notes_path,
        keep,
    )
    .is_ok()
}

/// [`accept_or_restore_with_snapshot`] with the reason of a rejection: `Err` is
/// the gate's own words (the rpmspec or patch complaint), already in the notes
/// and rolled back.
pub(super) fn accept_or_restore_reasoned(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_entry: &str,
    notes_path: PathBuf,
    keep: &[String],
) -> Result<(), String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());

    if let Some((line_no, text)) = crate::spec::first_macro_in_comment(&after) {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!(
                "spec comment at line {line_no} references a macro; \
                 RPM expands it during parse: {}",
                text.trim()
            ),
        });
    }

    match osc::rpmspec_check(spec_path, Emit::new(reporter, step.id)) {
        Ok(_) => {}
        // Only a verdict on the spec is a rejection. A check that could not run
        // (bwrap or rpmspec missing, a spawn failure, a timeout) says nothing
        // about the edit: it is journaled and the edit is kept.
        Err(e @ (osc::Error::RpmspecFailed { .. } | osc::Error::UnsandboxedMacro { .. })) => {
            note_spec_rejection(spec_path, &e.to_string());
            reporter.report(&Event::SpecRejected {
                step: step.id,
                reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                    stderr: e.to_string(),
                },
                restored_from: spec_path.to_path_buf(),
            });
            restore_spec(spec_path, before, Some(step), reporter);
            // When we have a full-checkout snapshot, roll back patches /
            // `.changes` / `_service` / etc. too — the agent's edits are an
            // atomic unit, and a half-reverted checkout is a worse state
            // than any version this run has actually validated.
            if let Some(snap) = snapshot {
                snap.restore_keeping(reporter, Some(step), keep);
            }
            append_notes(notes_path, &rollback_note(&e.to_string()));
            return Err(e.to_string());
        }
        Err(e) => {
            skipped_check_note(
                reporter,
                step,
                notes_path.clone(),
                "rpmspec",
                &e.to_string(),
            );
        }
    }

    // A patch header naming an absolute path or a `..` component would let the
    // patch carry files from outside the checkout into the commit. Checked for
    // every turn outcome, before anything else reads the patch.
    if let Some(problem) = reject_patch_escape(
        spec_path,
        before,
        snapshot,
        step,
        reporter,
        notes_path.clone(),
        keep,
    ) {
        return Err(problem);
    }

    // `rpmspec --parse` cannot see a patch that does not apply; that would only
    // surface as a `%prep` failure after a full OBS cycle. Hand-written patches
    // with wrong hunk counts ("malformed patch") are the usual culprit.
    let patch_check = check_patches_apply(spec_path, &after);
    if let Ok(Some(unchecked)) = &patch_check {
        skipped_check_note(reporter, step, notes_path.clone(), "patch", unchecked);
    }
    if let Err(problem) = patch_check {
        note_spec_rejection(spec_path, &problem);
        reporter.report(&Event::SpecRejected {
            step: step.id,
            reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                stderr: problem.clone(),
            },
            restored_from: spec_path.to_path_buf(),
        });
        restore_spec(spec_path, before, Some(step), reporter);
        if let Some(snap) = snapshot {
            snap.restore_keeping(reporter, Some(step), keep);
        }
        append_notes(
            notes_path,
            &format!(
                "\n## Edit rejected — a patch does not apply\n{problem}\n\
                 Create or change patches ONLY with quilt (`cd source && quilt new/add/refresh`), \
                 never by writing a .patch file by hand: hand-written hunk headers are wrong. \
                 The edit was rolled back.\n"
            ),
        );
        return Err(problem);
    }

    // A source declared again (already in the spec, or twice in this edit) is a
    // loop, not a change: the edit is rolled back before it reaches a build.
    if let Some(problem) = reject_repeated_sources(
        spec_path,
        before,
        snapshot,
        step,
        reporter,
        notes_path.clone(),
        keep,
    ) {
        return Err(problem);
    }

    clear_spec_rejections(spec_path);
    let diff = unified_diff(before, &after);
    let (added, removed) = diff_counts(&diff);
    let diff_path = step.file("spec.diff");
    write_or_warn(&diff_path, &diff);
    reporter.report(&Event::SpecChanged {
        step: step.id,
        diff,
        lines_added: added,
        lines_removed: removed,
        artifact: diff_path,
    });
    append_notes(notes_path, notes_entry);
    Ok(())
}

/// The journal entry for a turn whose edit was rejected before any build and
/// rolled back with all of its changes: without it the next turn sees an edit
/// listed nowhere and re-applies the same text (python-torch: ten identical
/// rejected turns).
pub(super) fn rollback_note(problem: &str) -> String {
    let problem: String = problem.trim().chars().take(600).collect();
    format!(
        "\n## Edit rejected — rpmspec failed, rolled back\n{problem}\n\
         The whole edit of that turn was undone, not only the line that failed. Do not \
         re-apply the same text; redo every change of the turn together with the corrected one.\n"
    )
}
