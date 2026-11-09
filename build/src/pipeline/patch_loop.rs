//! The patch loop: the patch stage with its turns.

use super::patch_block_quilt::drop_blocks_quilt_clears;
use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::patch_review::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::snapshot::*;

/// [`patch_stage`] with the quilt check and the turn runner given. The agent turns
/// record the patches as they were (see `begin_turn`); the record is dropped here,
/// once the stage has settled, however it ended.
pub(super) fn patch_stage_with(
    ctx: &RunCtx,
    st: &mut RunState,
    check: &mut PatchCheckFn<'_>,
    turn: &mut PatchTurn<'_>,
    review: &mut ReviewTurn<'_>,
) -> Result<(), PipelineResult> {
    let result = patch_stage_inner(ctx, st, check, turn, review);
    // The record stays when a stop could not put its rewrites back or record them.
    if !take_unsettled() {
        end_turn(&ctx.ws);
    }
    result
}

fn patch_stage_inner(
    ctx: &RunCtx,
    st: &mut RunState,
    check: &mut PatchCheckFn<'_>,
    turn: &mut PatchTurn<'_>,
    review: &mut ReviewTurn<'_>,
) -> Result<(), PipelineResult> {
    use crate::patch_check::{MAX_ROUNDS, NextRound, PatchStatus, Refresh};
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    let step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::ValidateSpec,
        Some("patches"),
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let step_id = step.id;
    let progress = |message: String| {
        reporter.report(&Event::Progress {
            step: Some(step_id),
            message,
        })
    };
    // One line per patch for the notes, so a later run knows what was done.
    let mut lines: Vec<String> = Vec::new();
    let write_notes = |lines: &[String], heading: &str| {
        if !lines.is_empty() {
            append_notes(
                ws.notes(),
                &format!(
                    "\n## Patches carried forward to {} ({heading})\n{}\n",
                    config.version,
                    lines.join("\n")
                ),
            );
        }
    };
    // The patches as OBS ships them: a patch whose code differs at the end was
    // rewritten by a turn and is reviewed. `accepted` holds the bytes a review
    // accepted, so a patch is not reviewed twice.
    let stage_start = patch_files(
        &std::fs::read_to_string(spec_path).unwrap_or_default(),
        checkout_dir,
    );
    let mut accepted: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
    let mut report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
    // A patch that applies with fuzz is refreshed here, by the pipeline, before
    // any turn (one with an offset alone is left as it is): the agent is handed
    // only the patches that have rejects.
    let mut refreshed: std::collections::BTreeSet<String> = Default::default();
    if report.to_refresh().next().is_some() {
        let offsets = check_and_settle(ctx, step_id, &mut lines, check, Refresh::Fuzz);
        refreshed.extend(write_refreshed_patches(ctx, &offsets, &mut lines));
        report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
    }
    // The upstream revisions of moved requests, before any rebase turn (plan 4.6).
    let failing: Vec<String> = report.failing().map(|p| p.name.clone()).collect();
    if super::patch_provenance::upstream_revisions_wanted(config, &failing) {
        let hosts = crate::allowed_hosts::load_default();
        let forge = crate::patch_provenance::LiveForge {
            package: rpm_name,
            hosts: &hosts,
        };
        // A revision taken before a stop keeps its refresh bullet.
        if let Err(stop) = super::patch_provenance::try_upstream_revisions(
            ctx,
            st,
            step_id,
            check,
            review,
            &forge,
            &failing,
            &mut accepted,
            &mut refreshed,
            &mut lines,
        ) {
            write_refresh_bullets(ctx, step_id, &refreshed);
            return Err(stop);
        }
        report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
    }
    if report.patches.is_empty() || report.not_run.is_some() || report.is_clean() {
        write_refresh_bullets(ctx, step_id, &refreshed);
        if report.not_run.is_some() {
            // Not a success: the patches are left to the `patch` check of ValidateSpec.
            reporter.report(&Event::Warning {
                step: Some(step_id),
                message: format!("{rpm_name}: {}", report.summary()),
            });
        } else if !report.patches.is_empty() {
            progress(format!(
                "{rpm_name}: the patches apply cleanly to {} ({})",
                config.version,
                report.summary()
            ));
        }
        write_notes(&lines, "quilt");
        step.finish(StepOutcome::Ok);
        return Ok(());
    }
    progress(format!("{rpm_name}: {}", report.summary()));
    // The top directory of an obs_scm source is a guess (nothing is unpacked), so
    // no patch is judged stale by it.
    let hints = if source_made_at_build_time(checkout_dir) {
        Vec::new()
    } else {
        stale_patches(
            &std::fs::read_to_string(spec_path).unwrap_or_default(),
            checkout_dir,
            &st.sdist_top_level,
            &config.version,
        )
    };
    let mut rounds = 0usize;
    // What the last round must hear: why its edit was refused.
    let mut feedback: Option<String> = None;
    // A patch the review rejected is waiting for another rebase round.
    let mut review_rejected = false;
    // The patches the agent blocked, each with its latest answer.
    let mut blocked: Vec<crate::agent::PatchAnswer> = Vec::new();
    let stop: String = if config.no_agent {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            st.attempts,
            st.spent_usd,
            &ctx.signals,
        );
        "--no-agent is set, so no agent turn rebased them".to_string()
    } else {
        loop {
            if crate::proc::shutting_down() {
                let settled = settle_unreviewed(ctx, &stage_start, &accepted);
                write_notes(&lines, "quilt, stopped");
                step.finish(StepOutcome::Failed);
                return Err(settle_stop(
                    ctx,
                    &settled,
                    ctx.failed_text(
                        true,
                        "stopped while rebasing the patches: the run is shutting down".into(),
                    ),
                ));
            }
            let budget_stop = ctx.may_start_turn(st.spent_usd).err();
            if st.attempts >= config.max_attempts || budget_stop.is_some() {
                skip_agent(
                    reporter,
                    &mut st.step_attempts,
                    config,
                    st.attempts,
                    st.spent_usd,
                    &ctx.signals,
                );
                break match budget_stop {
                    Some(reason) => format!("no agent turn left after {rounds} round(s): {reason}"),
                    None => format!(
                        "no agent turn left after {rounds} round(s): {} of {} attempts, ${:.2} of ${:.2}",
                        st.attempts, config.max_attempts, st.spent_usd, config.budget_usd
                    ),
                };
            }
            rounds += 1;
            st.attempts += 1;
            persist_attempts(ws, &config.run_id, st.attempts);
            let before_spec = std::fs::read_to_string(spec_path).unwrap_or_default();
            let before_files = patch_files(&before_spec, checkout_dir);
            // Dropped at the end of this round: the file is never left behind.
            let (_series, series_notes) = write_rebase_series(&before_spec, checkout_dir);
            if rounds == 1 {
                for note in &series_notes {
                    progress(format!("{rpm_name}: {note}"));
                    lines.push(format!("- series: {note}"));
                }
            }
            pop_source_patches(ctx);
            let snapshot = CheckoutSnapshot::take(checkout_dir);
            let context = format!(
                "Round {rounds} of at most {MAX_ROUNDS}.\n\n{}{}",
                feedback
                    .take()
                    .map(|f| format!("Your last edit was rolled back: {f}\n\n"))
                    .unwrap_or_default(),
                crate::patch_check::agent_report(&report, &config.version, &hints, checkout_dir)
            );
            let (turn_step, outcome) = match turn(ctx, st, st.attempts, &context) {
                Ok(v) => v,
                Err((e, kind)) => {
                    // The edits quilt proves apply stay; the rest of the turn is
                    // rolled back, and what still fails is reported per patch.
                    let stopped = format!("the agent turn failed: {e}");
                    let kept = keep_proven_edits(
                        ctx,
                        step_id,
                        check,
                        &mut lines,
                        &snapshot,
                        &before_spec,
                        &before_files,
                    );
                    // A kept edit that is a rewrite is put back below, never reviewed:
                    // the stage does not claim it, nor give it a bullet.
                    let settled = settle_unreviewed(ctx, &stage_start, &accepted);
                    let undone = settled.as_ref().cloned().unwrap_or_default();
                    let kept: Vec<String> =
                        kept.into_iter().filter(|n| !undone.contains(n)).collect();
                    for n in &undone {
                        lines.push(format!(
                            "- {n}: put back; the agent's rewrite was not reviewed"
                        ));
                    }
                    let kept_any = !kept.is_empty();
                    refreshed.extend(kept);
                    pop_source_patches(ctx);
                    let report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
                    drop_blocks_quilt_clears(&mut blocked, &report, &mut lines);
                    // The rounds before this one keep their refreshed patches.
                    write_refresh_bullets(ctx, step_id, &refreshed);
                    let failing: Vec<&crate::patch_check::PatchResult> = report.failing().collect();
                    for p in &failing {
                        lines.push(format!(
                            "- {}: still does not apply; kept ({stopped})",
                            p.name
                        ));
                    }
                    write_notes(&lines, "quilt, stopped");
                    step.finish(StepOutcome::Failed);
                    let reason = if failing.is_empty() && blocked.is_empty() {
                        if kept_any {
                            format!("{stopped}; the edits quilt proves apply were kept")
                        } else {
                            format!("{stopped}; no edit was kept")
                        }
                    } else {
                        format!(
                            "{stopped}; {}",
                            kept_patches_reason(
                                &config.version,
                                "the rest were rolled back",
                                &failing,
                                &blocked
                            )
                        )
                    };
                    return Err(settle_stop(ctx, &settled, ctx.failed_as(kind, reason)));
                }
            };
            let applied =
                apply_patch_turn(ctx, &turn_step, &outcome, &before_spec, &snapshot, rounds);
            for a in &outcome.patch_answers {
                lines.push(format!(
                    "- round {rounds}: {} {:?}: {}",
                    a.name, a.status, a.reason
                ));
            }
            record_patch_answers(&mut blocked, &outcome.patch_answers);
            pop_source_patches(ctx);
            let changed = settle_patch_changes(
                ctx,
                step_id,
                &before_spec,
                &before_files,
                rounds,
                &mut lines,
                &mut refreshed,
            );
            // A refused removal goes back to the agent with the reason: that is
            // a round worth having, within the bound.
            let refused = if let PatchTurnApplied::Refused(why) = applied {
                lines.push(format!("- round {rounds}: {why}"));
                feedback = Some(why);
                true
            } else {
                false
            };
            let next = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
            // The round stalls when every patch still failing was blocked in it
            // and no file is different from before it: another turn would be
            // given the same patches and answer the same way.
            let after_spec = std::fs::read_to_string(spec_path).unwrap_or_default();
            let unchanged =
                after_spec == before_spec && patch_files(&after_spec, checkout_dir) == before_files;
            let remaining: Vec<String> = next.failing().map(|p| p.name.clone()).collect();
            let stalled = all_blocked_stall(&remaining, &outcome.patch_answers, unchanged);
            let verdict = crate::patch_check::next_round(
                rounds,
                MAX_ROUNDS,
                report.tally(),
                next.tally(),
                changed || refused,
            );
            progress(format!(
                "{rpm_name}: after patch round {rounds}: {}",
                next.summary()
            ));
            report = next;
            drop_blocks_quilt_clears(&mut blocked, &report, &mut lines);
            if stalled {
                break format!(
                    "round {rounds} blocked every remaining patch and changed no file, so \
                     another turn would repeat it"
                );
            }
            match verdict {
                NextRound::Again => {}
                NextRound::Clean => {
                    // A blocked patch ends the package below, reviewed or not.
                    if !blocked.is_empty() {
                        break String::new();
                    }
                    match review_stage_patches(
                        ctx,
                        st,
                        step_id,
                        check,
                        review,
                        &stage_start,
                        &mut accepted,
                        &mut lines,
                        rounds >= MAX_ROUNDS,
                    ) {
                        Err(result) => {
                            let settled = settle_unreviewed(ctx, &stage_start, &accepted);
                            write_refresh_bullets(ctx, step_id, &refreshed);
                            write_notes(&lines, "quilt, review stopped");
                            step.finish(StepOutcome::Failed);
                            return Err(settle_stop(ctx, &settled, result));
                        }
                        Ok(ReviewEnd::Settled) => break String::new(),
                        Ok(ReviewEnd::Redo(why)) => {
                            lines.push(format!("- round {rounds}: {why}"));
                            feedback = Some(why);
                            review_rejected = true;
                            report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
                            drop_blocks_quilt_clears(&mut blocked, &report, &mut lines);
                        }
                    }
                }
                NextRound::NoProgress => break format!("round {rounds} made no progress"),
                NextRound::OutOfRounds => break format!("{rounds} round(s) did not fix them"),
            }
        }
    };
    // A patch an agent turn left with fuzz (which fails rpm's `--fuzz=0`) is refreshed the same way the pre-pass refreshed the rest.
    if report.to_refresh().next().is_some() {
        let offsets = check_and_settle(ctx, step_id, &mut lines, check, Refresh::Fuzz);
        refreshed.extend(write_refreshed_patches(ctx, &offsets, &mut lines));
    }
    write_refresh_bullets(ctx, step_id, &refreshed);
    for p in &report.patches {
        match &p.status {
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }
                if !refreshed.contains(&p.name) =>
            {
                lines.push(format!(
                    "- {}: applies with fuzz and quilt could not refresh it; left as it is",
                    p.name
                ))
            }
            PatchStatus::NotChecked { reason } => {
                lines.push(format!("- {}: not checked: {reason}", p.name))
            }
            _ => {}
        }
    }
    // What still fails is kept, never dropped: the package stops for a human.
    let failing: Vec<&crate::patch_check::PatchResult> = report.failing().collect();
    // A patch the agent blocked ends the package too, whatever quilt says, unless
    // quilt shows it applies cleanly.
    drop_blocks_quilt_clears(&mut blocked, &report, &mut lines);
    if !failing.is_empty() || !blocked.is_empty() {
        for p in &failing {
            lines.push(format!("- {}: still does not apply; kept ({stop})", p.name));
        }
        let settled = settle_unreviewed(ctx, &stage_start, &accepted);
        write_notes(&lines, "quilt");
        step.finish(StepOutcome::Failed);
        let reason = kept_patches_reason(&config.version, &stop, &failing, &blocked);
        // A patch the review rejected and no round is left to redo needs a maintainer.
        let result = if review_rejected {
            ctx.failed_as(FailureKind::NeedsMaintainer, reason)
        } else {
            ctx.failed(reason)
        };
        return Err(settle_stop(ctx, &settled, result));
    }
    // A loop that ended another way (no progress, out of rounds) can leave a
    // rewritten patch that applies, with fuzz now refreshed: it is reviewed too,
    // with no round left to redo it.
    if !config.no_agent
        && let Err(result) = review_stage_patches(
            ctx,
            st,
            step_id,
            check,
            review,
            &stage_start,
            &mut accepted,
            &mut lines,
            true,
        )
    {
        let settled = settle_unreviewed(ctx, &stage_start, &accepted);
        write_notes(&lines, "quilt, review stopped");
        step.finish(StepOutcome::Failed);
        return Err(settle_stop(ctx, &settled, result));
    }
    write_notes(&lines, "quilt");
    step.finish(StepOutcome::Ok);
    Ok(())
}

/// Whether a round stalls: every patch in `remaining` (the ones still failing
/// after it) has a `blocked` answer in `answers`, and the round changed no file
/// (`unchanged`). An empty `remaining` never stalls.
pub(super) fn all_blocked_stall(
    remaining: &[String],
    answers: &[crate::agent::PatchAnswer],
    unchanged: bool,
) -> bool {
    use crate::agent::PatchAnswerStatus::Blocked;
    unchanged
        && !remaining.is_empty()
        && remaining.iter().all(|name| {
            answers
                .iter()
                .any(|a| &a.name == name && a.status == Blocked)
        })
}

/// Folds one turn's per-patch answers into `blocked`: each patch keeps only its
/// latest answer, so a patch that a later round ported leaves the list.
pub(super) fn record_patch_answers(
    blocked: &mut Vec<crate::agent::PatchAnswer>,
    answers: &[crate::agent::PatchAnswer],
) {
    for a in answers {
        blocked.retain(|b| b.name != a.name);
        if a.status == crate::agent::PatchAnswerStatus::Blocked {
            blocked.push(a.clone());
        }
    }
}

/// Writes each patch of `report` that quilt refreshed into the checkout, with a
/// note for each. Returns the names written.
fn write_refreshed_patches(
    ctx: &RunCtx,
    report: &crate::patch_check::CheckReport,
    lines: &mut Vec<String>,
) -> Vec<String> {
    let mut names = Vec::new();
    for p in &report.patches {
        if let Some(bytes) = &p.refreshed {
            write_or_warn(ctx.checkout_dir.join(&p.name), bytes);
            lines.push(format!(
                "- {}: refreshed by the pipeline (it applied with fuzz)",
                p.name
            ));
            names.push(p.name.clone());
        }
    }
    names
}
