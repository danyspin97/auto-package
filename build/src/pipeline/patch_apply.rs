//! Applying a RefreshPatches turn's outcome: the edit is kept or rolled back, and
//! a patch the turn adds meets the patch rules and is named in `.changes`.

use super::*;
use crate::pipeline::patch_rules::*;
use crate::pipeline::rejection::*;
use crate::pipeline::removal_proof::*;
use crate::pipeline::render::*;
use crate::pipeline::snapshot::*;

/// How a RefreshPatches turn's outcome was applied.
#[derive(Debug, PartialEq)]
pub(super) enum PatchTurnApplied {
    /// The edit was kept.
    Kept,
    /// Nothing was kept: no edit, or a spec rpmspec refuses.
    RolledBack,
    /// The edit was rolled back: it removed patches without proof, or a patch it
    /// adds breaks a patch rule or is not named in `.changes`. What to tell the
    /// next round.
    Refused(String),
}

/// Apply a RefreshPatches turn's outcome. An edit is kept when the spec still
/// parses, no patch escapes the tree and every patch it removed is proven
/// already applied; whether the patches apply is the next quilt check's to say
/// (the usual gate would refuse the edit while any patch still fails, which
/// would throw away a round that fixed one of several). Anything else is
/// rolled back.
pub(super) fn apply_patch_turn(
    ctx: &RunCtx,
    step: &StepDir,
    outcome: &crate::agent::TurnOutcome,
    before: &str,
    snapshot: &CheckoutSnapshot,
    round: usize,
) -> PatchTurnApplied {
    let (reporter, spec_path, notes) = (ctx.reporter, &ctx.spec_path, ctx.ws.notes());
    let label = format!("RefreshPatches — round {round}");
    // With a per-patch answer, the patches it names as rebased or removed keep
    // their edits and every other edit is rolled back. Without one, the action
    // decides, as it always did.
    let answers = &outcome.patch_answers;
    let kept_by_answer: Vec<String> = answers
        .iter()
        .filter(|a| a.status != crate::agent::PatchAnswerStatus::Blocked)
        .map(|a| a.name.clone())
        .collect();
    let edited = if answers.is_empty() {
        matches!(outcome.action, crate::agent::Action::Edited)
    } else {
        !kept_by_answer.is_empty()
    };
    if !edited {
        snapshot.restore_keeping(reporter, Some(step), &[]);
        restore_spec(spec_path, before, Some(step), reporter);
        append_notes(
            notes,
            &format!(
                "\n## {label} — no change\nReasoning: {}\n{}",
                brief(&outcome.reasoning),
                answer_lines(answers)
            ),
        );
        return PatchTurnApplied::RolledBack;
    }
    if !answers.is_empty() {
        snapshot.restore_keeping(reporter, Some(step), &kept_by_answer);
        // A removal is a spec edit too; without one the spec goes back.
        if !answers
            .iter()
            .any(|a| a.status == crate::agent::PatchAnswerStatus::Removed)
        {
            restore_spec(spec_path, before, Some(step), reporter);
        }
    }
    match osc::rpmspec_check(spec_path, Emit::new(reporter, step.id)) {
        Ok(_) => {}
        Err(e @ (osc::Error::RpmspecFailed { .. } | osc::Error::UnsandboxedMacro { .. })) => {
            restore_spec(spec_path, before, Some(step), reporter);
            snapshot.restore_keeping(reporter, Some(step), &[]);
            append_notes(notes, &rollback_note(&e.to_string()));
            return PatchTurnApplied::RolledBack;
        }
        Err(e) => skipped_check_note(reporter, step, notes.clone(), "rpmspec", &e.to_string()),
    }
    if reject_patch_escape(
        spec_path,
        before,
        Some(snapshot),
        step,
        reporter,
        notes.clone(),
        &[],
    )
    .is_some()
    {
        return PatchTurnApplied::RolledBack;
    }
    let judged = judge_removals(
        &ctx.config.version,
        &ctx.checkout_dir,
        spec_path,
        before,
        Some(snapshot),
    );
    let unproven = judged.refused;
    if !unproven.is_empty() {
        return PatchTurnApplied::Refused(reject_unproven_removal(
            spec_path,
            before,
            Some(snapshot),
            step,
            reporter,
            notes,
            &[],
            &label,
            &unproven,
        ));
    }
    // A patch the turn adds meets the rules of a fix turn's (a header, a PATCH comment,
    // source-only changes) and is named in `.changes` by the bullet the turn gave.
    let bullet_source = turn_bullet_source(outcome);
    if let Some(why) = reject_patch_rules(
        spec_path,
        before,
        Some(snapshot),
        step,
        reporter,
        notes.clone(),
        &[],
        true,
    ) {
        return PatchTurnApplied::Refused(why);
    }
    if let Some(why) = reject_unnamed_patches(
        spec_path,
        before,
        Some(snapshot),
        step,
        reporter,
        notes.clone(),
        &[],
        &bullet_source,
        &label,
    ) {
        return PatchTurnApplied::Refused(why);
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    if !added_patches(spec_path, before, &after, Some(snapshot)).is_empty() {
        snapshot.restore_changes_files(reporter, Some(step));
        add_edit_changes_entry(
            ctx.config,
            &ctx.checkout_dir,
            reporter,
            step,
            &bullet_source,
        );
    }
    append_notes(
        notes,
        &format!(
            "\n## {label}\nReasoning: {}\n{}{}",
            outcome.reasoning,
            evidence_lines(&outcome.evidence),
            answer_lines(answers)
        ),
    );
    PatchTurnApplied::Kept
}

/// One line per patch a RefreshPatches turn answered, for the notes.
fn answer_lines(answers: &[crate::agent::PatchAnswer]) -> String {
    answers
        .iter()
        .map(|a| format!("- {} ({:?}): {}\n", a.name, a.status, a.reason))
        .collect()
}
