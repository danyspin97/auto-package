//! The review of one rewritten patch: fresh turns, at most [`MAX_REVIEW_ROUNDS`],
//! each holding the reviewer to the one patch file and gating a rewrite.

use super::*;
use crate::pipeline::patch_refresh::status_reason;
use crate::pipeline::patch_review_prompt::*;
use crate::pipeline::patch_rules::*;
use crate::pipeline::rejection::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// The turns one patch gets: a rewrite in the last round is rolled back.
pub(super) const MAX_REVIEW_ROUNDS: usize = 3;

/// Runs one PatchReview turn for the patch `&str` with the Context `&str`.
pub(super) type ReviewTurn<'a> = dyn FnMut(
        &RunCtx,
        &mut RunState,
        &str,
        &str,
    ) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)>
    + 'a;

/// Whether the patch `&str` applies to the new sources as it is now; `Err` is why not.
pub(super) type ApplyCheck<'a> = dyn FnMut(&RunCtx, &str) -> Result<(), String> + 'a;

/// What one review answer means once the bytes are known.
#[derive(Debug, PartialEq)]
pub(super) enum ReviewMove {
    Accept,
    /// The patch file changed: the bytes decide, whatever the verdict says.
    Rewrite,
    Reject(String),
    GiveUp(String),
    /// The answer and the bytes disagree, or there is no verdict.
    Slip(String),
}

/// The move an answer makes. `changed`: the patch file differs from before the
/// turn (after the pipeline held the turn to that file).
pub(super) fn review_move(outcome: &crate::agent::TurnOutcome, changed: bool) -> ReviewMove {
    use crate::agent::PatchReviewVerdict as V;
    if let crate::agent::Action::Blocked { why } = &outcome.action {
        let why = if why.is_empty() {
            &outcome.reasoning
        } else {
            why
        };
        return ReviewMove::GiveUp(why.clone());
    }
    let Some(review) = &outcome.patch_review else {
        return ReviewMove::Slip("the answer has no `review`".into());
    };
    match (review.verdict, changed) {
        (V::Accept, false) => ReviewMove::Accept,
        (V::Accept | V::Rewrite, true) => ReviewMove::Rewrite,
        (V::Rewrite, false) => {
            ReviewMove::Slip("you answered rewrite but the patch is unchanged".into())
        }
        (V::Reject, _) => {
            let mut why = review.reason.clone();
            for p in &review.problems {
                why.push_str(&format!("; {p}"));
            }
            ReviewMove::Reject(why)
        }
    }
}

/// The production [`ReviewTurn`]: a fresh PatchReview turn on the patch `name`,
/// which it may write and no other file of the checkout.
pub(super) fn agent_review_turn(
    ctx: &RunCtx,
    st: &mut RunState,
    name: &str,
    context: &str,
) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
    let names = [name.to_string()];
    // A session of its own, never kept: the fix turns resume `st.session_id`,
    // and a review must not take the writer's conversation from them.
    let mut session: Option<String> = None;
    crate::pipeline::agent::agent_turn(
        crate::pipeline::agent::TurnCtx {
            carried: false,
            label: "patch-review",
            task: crate::agent::Task::PatchReview,
            event_task: ap_core::event::Task::PatchReview,
            attempt: st.attempts,
            hint: None,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(&ctx.checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: false,
            resolver: false,
            context: Some(context),
        },
        crate::pipeline::agent::TurnEnv {
            writable_patches: &names,
            ..ctx.turn_env()
        },
        &mut st.spent_usd,
        &mut session,
        &mut st.step_attempts,
    )
}

/// How the review of one patch ended.
#[derive(Debug, PartialEq)]
pub(super) enum PatchReviewEnd {
    /// `rewrote`: the reviewer itself changed the patch on the way.
    Accepted {
        rewrote: bool,
    },
    Rejected(String),
    /// The rounds ran out with no accept.
    Unsettled(String),
    GaveUp(String),
}

/// The copy of the patch before the rewrite that the turn can read, removed when
/// it drops.
struct OrigCopy(PathBuf);

impl Drop for OrigCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The top-level checkout files other than `name` that differ from `snapshot`:
/// changed, added or deleted. The review may change `name` only.
fn out_of_scope(snapshot: &CheckoutSnapshot, name: &str) -> Vec<String> {
    let dir = &snapshot.checkout_dir;
    let mut out: Vec<String> = Vec::new();
    for (file, entry) in &snapshot.files {
        let (Some(file), Some(old)) = (file.to_str(), &entry.content) else {
            continue;
        };
        if file != name && std::fs::read(dir.join(file)).ok().as_ref() != Some(old) {
            out.push(file.to_string());
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let file = e.file_name();
            if e.file_type().is_ok_and(|t| t.is_file())
                && !snapshot.files.contains_key(&file)
                && file.to_str().is_some_and(|f| f != name)
            {
                out.push(file.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// What a round is told about the files its turn touched besides its patch.
fn scope_note(files: &[String], name: &str) -> Option<String> {
    (!files.is_empty()).then(|| {
        format!(
            "only {name} may change; your edit to {} was rolled back",
            files.join(", ")
        )
    })
}

/// Records one round's verdict: the event, the notes and a line for the stage.
#[allow(clippy::too_many_arguments)]
fn record_round(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    name: &str,
    round: usize,
    verdict: &str,
    reason: &str,
    lines: &mut Vec<String>,
) {
    ctx.reporter.report(&Event::PatchReviewed {
        step,
        pkg: ctx.config.rpm_name.clone(),
        patch: name.to_string(),
        round: round as u32,
        verdict: verdict.to_string(),
        reason: reason.to_string(),
    });
    append_notes(
        ctx.ws.notes(),
        &format!("\n## PatchReview — {name} — round {round}\nVerdict: {verdict}\n{reason}\n"),
    );
    lines.push(format!(
        "- {name}: review round {round}: {verdict}: {reason}"
    ));
}

/// Gates the reviewer's rewrite, rolling it back with its reason when it fails:
/// no path escapes the tree, the patch rules hold on what the rewrite added, and
/// the patch still applies.
fn gate_rewrite(
    ctx: &RunCtx,
    step: &StepDir,
    name: &str,
    spec_before: &str,
    snapshot: &CheckoutSnapshot,
    applies: &mut ApplyCheck<'_>,
) -> Option<String> {
    let (reporter, spec_path) = (ctx.reporter, &ctx.spec_path);
    let notes = ctx.ws.notes();
    if let Some(why) = reject_patch_escape(
        spec_path,
        spec_before,
        Some(snapshot),
        step,
        reporter,
        notes.clone(),
        &[],
    ) {
        return Some(why);
    }
    if let Some(why) = reject_patch_rules(
        spec_path,
        spec_before,
        Some(snapshot),
        step,
        reporter,
        notes,
        &[],
        false,
    ) {
        return Some(why);
    }
    if let Err(why) = applies(ctx, name) {
        snapshot.restore(reporter, Some(step));
        restore_spec(spec_path, spec_before, Some(step), reporter);
        return Some(format!("the rewrite does not apply: {why}"));
    }
    None
}

/// Reviews the patch `name` for at most [`MAX_REVIEW_ROUNDS`] rounds. `original`
/// is the patch before the rewrite (`None`: the patch is new), `cause` says what
/// changed it. `Err` stops the package: the run is shutting down, no turn can
/// start (`Exhausted`) or the turn failed.
#[allow(clippy::too_many_arguments)]
pub(super) fn review_patch_rounds(
    ctx: &RunCtx,
    st: &mut RunState,
    step_id: ap_core::event::StepId,
    name: &str,
    original: Option<&[u8]>,
    cause: &str,
    applies: &mut ApplyCheck<'_>,
    review: &mut ReviewTurn<'_>,
    lines: &mut Vec<String>,
) -> Result<PatchReviewEnd, PipelineResult> {
    let RunCtx {
        reporter,
        ws,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    // A name from the spec can widen a tool grant: it is never reviewed.
    if !crate::agent::safe_review_patch_name(name) {
        let why = "its name is not a plain file name a review may be granted";
        record_round(ctx, step_id, name, 1, "not_reviewed", why, lines);
        return Err(ctx.failed_as(
            FailureKind::NeedsMaintainer,
            format!("{name} was rewritten and {why}; it needs a maintainer"),
        ));
    }
    let patch_path = checkout_dir.join(name);
    let orig_text = original.map(|o| String::from_utf8_lossy(o).into_owned());
    let mut rollback: Option<String> = None;
    let mut rewrote = false;
    let mut last_why = String::new();
    for round in 1..=MAX_REVIEW_ROUNDS {
        if crate::proc::shutting_down() {
            return Err(ctx.failed_text(
                true,
                format!("stopped while reviewing {name}: the run is shutting down"),
            ));
        }
        if let Err(why) = ctx.may_start_turn(st.spent_usd) {
            record_round(ctx, step_id, name, round, "not_reviewed", &why, lines);
            return Err(ctx.failed_as(
                FailureKind::Exhausted,
                format!("{name} was rewritten and no turn is left to review it: {why}"),
            ));
        }
        let spec_before = std::fs::read_to_string(spec_path).unwrap_or_default();
        // Dropped at the end of the round: neither file is left behind.
        let (_series, _) = write_rebase_series(&spec_before, checkout_dir);
        pop_source_patches(ctx);
        let copy_dir = ws.dir().join("patch-review");
        let _ = std::fs::create_dir_all(&copy_dir);
        let _orig = original.map(|o| {
            let path = copy_dir.join(format!("{name}.orig"));
            write_or_warn(&path, o);
            OrigCopy(path)
        });
        let quilt = match applies(ctx, name) {
            Ok(()) => "the patch applies".to_string(),
            Err(why) => format!("the patch does not apply: {why}"),
        };
        let before_bytes = std::fs::read(&patch_path).unwrap_or_default();
        let current = String::from_utf8_lossy(&before_bytes).into_owned();
        let mut targets = crate::pipeline::tarball::patch_targets(&current);
        targets.extend(
            orig_text
                .iter()
                .flat_map(|t| crate::pipeline::tarball::patch_targets(t)),
        );
        targets.sort();
        targets.dedup();
        let number = crate::patch_check::declared_patches(&spec_before)
            .into_iter()
            .find(|(_, n)| n == name)
            .map(|(n, _)| n)
            .unwrap_or_default();
        let comment = spec_patch_comment(&spec_before, name);
        let context = patch_review_context(&ReviewInput {
            round,
            name,
            level: spec_patch_level(&spec_before, &number),
            spec_comment: comment.as_deref(),
            cause,
            targets: &targets,
            quilt: &quilt,
            original: orig_text.as_deref(),
            current: &current,
            rollback: rollback.as_deref(),
        });
        let snapshot = CheckoutSnapshot::take(checkout_dir);
        let reply = review(ctx, st, name, &context);
        // The reviewer's pushed patches and edits in source/ do not outlive it.
        pop_source_patches(ctx);
        let (step, outcome) = match reply {
            Ok(v) => v,
            Err((e, kind)) => {
                snapshot.restore(reporter, None);
                restore_spec(spec_path, &spec_before, None, reporter);
                return Err(ctx.failed_as(kind, format!("the review of {name} failed: {e}")));
            }
        };
        // The review changes its patch file and nothing else.
        let stray = out_of_scope(&snapshot, name);
        snapshot.restore_keeping(reporter, Some(&step), &[name.to_string()]);
        restore_spec(spec_path, &spec_before, Some(&step), reporter);
        let changed = std::fs::read(&patch_path).unwrap_or_default() != before_bytes;
        let stray_note = scope_note(&stray, name);
        let reason = outcome
            .patch_review
            .as_ref()
            .map_or_else(|| outcome.reasoning.clone(), |r| r.reason.clone());
        rollback = stray_note.clone();
        // What the next round hears: the stray edit and the round's own reason.
        let tell = |why: &str| match &stray_note {
            Some(note) => format!("{note}; {why}"),
            None => why.to_string(),
        };
        let last = round == MAX_REVIEW_ROUNDS;
        match review_move(&outcome, changed) {
            ReviewMove::Accept => {
                record_round(ctx, step_id, name, round, "accepted", &reason, lines);
                return Ok(PatchReviewEnd::Accepted { rewrote });
            }
            ReviewMove::GiveUp(why) => {
                snapshot.restore(reporter, Some(&step));
                record_round(ctx, step_id, name, round, "gave_up", &why, lines);
                return Ok(PatchReviewEnd::GaveUp(why));
            }
            ReviewMove::Reject(why) => {
                snapshot.restore(reporter, Some(&step));
                record_round(ctx, step_id, name, round, "rejected", &why, lines);
                return Ok(PatchReviewEnd::Rejected(why));
            }
            ReviewMove::Rewrite if last => {
                snapshot.restore(reporter, Some(&step));
                last_why = "a rewrite in the last round is rolled back".to_string();
                record_round(ctx, step_id, name, round, "rolled_back", &last_why, lines);
            }
            ReviewMove::Rewrite => {
                match gate_rewrite(ctx, &step, name, &spec_before, &snapshot, applies) {
                    Some(why) => {
                        record_round(ctx, step_id, name, round, "rolled_back", &why, lines);
                        rollback = Some(tell(&why));
                    }
                    None => {
                        rewrote = true;
                        record_round(ctx, step_id, name, round, "rewritten", &reason, lines);
                    }
                }
            }
            ReviewMove::Slip(why) => {
                if changed {
                    snapshot.restore(reporter, Some(&step));
                }
                record_round(ctx, step_id, name, round, "slip", &why, lines);
                rollback = Some(tell(&why));
                last_why = why;
            }
        }
    }
    Ok(PatchReviewEnd::Unsettled(last_why))
}

/// Whether the patch stage may go on after the reviews.
#[derive(Debug, PartialEq)]
pub(super) enum ReviewEnd {
    /// Every rewritten patch was accepted.
    Settled,
    /// A patch was rejected and is back at its pre-rewrite bytes, so it fails
    /// quilt again: the refresh turn is told these reasons.
    Redo(String),
}

/// Whether `name` applies to the new sources, by the quilt check of the stage. A
/// patch that applies with an offset or fuzz is refreshed in place first.
pub(super) fn stage_applies(
    ctx: &RunCtx,
    check: &mut PatchCheckFn<'_>,
    name: &str,
) -> Result<(), String> {
    use crate::patch_check::{PatchStatus, Refresh};
    let report = check(ctx, Refresh::No);
    let Some(patch) = report.patches.iter().find(|p| p.name == name) else {
        return Err("the patch is not in the quilt check".into());
    };
    match &patch.status {
        PatchStatus::Applies => Ok(()),
        PatchStatus::AppliesWithOffsetOrFuzz { .. } => {
            let refreshed = check(ctx, Refresh::Fuzz);
            if let Some(bytes) = refreshed
                .patches
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| p.refreshed.as_ref())
            {
                write_or_warn(ctx.checkout_dir.join(name), bytes);
            }
            Ok(())
        }
        status => Err(status_reason(status)),
    }
}

/// Puts a patch back to the bytes it had before the stage rewrote it. A patch
/// the stage made has no earlier bytes and stays.
fn restore_original(ctx: &RunCtx, name: &str, original: Option<&[u8]>) {
    if let Some(bytes) = original {
        write_or_warn(ctx.checkout_dir.join(name), bytes);
    }
}

/// Reviews every patch the agent rewrote in this stage (`stage_start` is the
/// patches as they came; a refresh that only moved context or offsets is no
/// rewrite). Every reviewed patch is kept in `accepted`, so a redo does not
/// review it again. `last`: no refresh round is left to redo a rejected patch.
/// `Err` stops the package; the patch that stopped it is back at its
/// pre-rewrite bytes.
#[allow(clippy::too_many_arguments)]
pub(super) fn review_stage_patches(
    ctx: &RunCtx,
    st: &mut RunState,
    step_id: ap_core::event::StepId,
    check: &mut PatchCheckFn<'_>,
    review: &mut ReviewTurn<'_>,
    stage_start: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    accepted: &mut std::collections::BTreeMap<String, Vec<u8>>,
    lines: &mut Vec<String>,
    last: bool,
) -> Result<ReviewEnd, PipelineResult> {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let names = crate::pipeline::patch_rewrite::rewritten_patches(
        &spec,
        &ctx.checkout_dir,
        stage_start,
        accepted,
    );
    let mut redo: Vec<String> = Vec::new();
    for name in names {
        let original = stage_start.get(&name).and_then(|o| o.as_deref());
        let cause = if original.is_some() {
            format!("rebased onto {} by a refresh turn", ctx.config.version)
        } else {
            "added by a refresh turn".to_string()
        };
        let end = review_patch_rounds(
            ctx,
            st,
            step_id,
            &name,
            original,
            &cause,
            &mut |ctx, n| stage_applies(ctx, check, n),
            review,
            lines,
        );
        let stop = |why: String| {
            restore_original(ctx, &name, original);
            ctx.failed_as(FailureKind::NeedsMaintainer, why)
        };
        match end {
            Err(failed) => {
                restore_original(ctx, &name, original);
                return Err(failed);
            }
            Ok(PatchReviewEnd::Accepted { rewrote }) => {
                let bytes = std::fs::read(ctx.checkout_dir.join(&name)).unwrap_or_default();
                accepted.insert(name.clone(), bytes);
                // A patch the reviewer rewrote stays marked when the pipeline's own
                // refresh later changes its bytes and it is reviewed again.
                if (rewrote || was_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, &name))
                    && let Err(why) = mark_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, &name)
                {
                    return Err(ctx.failed_as(FailureKind::NeedsMaintainer, why));
                }
            }
            Ok(PatchReviewEnd::Rejected(why)) if original.is_some() && !last => {
                restore_original(ctx, &name, original);
                redo.push(format!(
                    "the patch review rejected {name}:\n{}",
                    crate::agent::untrusted_data(
                        "patch review reasons",
                        &crate::patch_check::truncate(&why, 2_000)
                    )
                ));
            }
            Ok(PatchReviewEnd::Rejected(why)) => {
                return Err(stop(format!(
                    "the patch review rejected {name}{}: {why}; it needs a maintainer",
                    if original.is_none() {
                        " (a patch new in this update)"
                    } else {
                        " in the last round"
                    }
                )));
            }
            Ok(PatchReviewEnd::GaveUp(why)) => {
                return Err(stop(format!(
                    "the patch review gave up on {name}: {why}; it needs a maintainer"
                )));
            }
            Ok(PatchReviewEnd::Unsettled(why)) => {
                return Err(stop(format!(
                    "the patch review did not settle {name} in {MAX_REVIEW_ROUNDS} rounds \
                     ({why}); it needs a maintainer"
                )));
            }
        }
    }
    Ok(if redo.is_empty() {
        ReviewEnd::Settled
    } else {
        ReviewEnd::Redo(redo.join("\n"))
    })
}
