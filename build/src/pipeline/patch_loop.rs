//! The patch loop: the patch stage with its turns, and the empty submodule and
//! wheel outcome handling.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::python_spec::*;
use crate::pipeline::render::*;
use crate::pipeline::snapshot::*;

/// [`patch_stage`] with the quilt check and the turn runner given.
pub(super) fn patch_stage_with(
    ctx: &RunCtx,
    st: &mut RunState,
    check: &mut PatchCheckFn<'_>,
    turn: &mut PatchTurn<'_>,
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
    let mut report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
    // A patch that applies only with an offset or fuzz is refreshed here, by
    // the pipeline, before any turn: the agent is handed only the patches that
    // have rejects.
    let mut refreshed: std::collections::BTreeSet<String> = Default::default();
    if report.to_refresh().next().is_some() {
        let offsets = check_and_settle(ctx, step_id, &mut lines, check, Refresh::Offset);
        refreshed.extend(write_refreshed_patches(ctx, &offsets, &mut lines));
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
    // The patches the agent blocked, each with its latest answer.
    let mut blocked: Vec<crate::agent::PatchAnswer> = Vec::new();
    let stop: String = if config.no_agent {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            st.attempts,
            st.spent_usd,
        );
        "--no-agent is set, so no agent turn rebased them".to_string()
    } else {
        loop {
            if crate::proc::shutting_down() {
                write_notes(&lines, "quilt, stopped");
                step.finish(StepOutcome::Failed);
                return Err(ctx.failed_text(
                    true,
                    "stopped while rebasing the patches: the run is shutting down".into(),
                ));
            }
            if st.attempts >= config.max_attempts || st.spent_usd >= config.budget_usd {
                skip_agent(
                    reporter,
                    &mut st.step_attempts,
                    config,
                    st.attempts,
                    st.spent_usd,
                );
                break format!(
                    "no agent turn left after {rounds} round(s): {} of {} attempts, ${:.2} of ${:.2}",
                    st.attempts, config.max_attempts, st.spent_usd, config.budget_usd
                );
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
                    refreshed.extend(kept);
                    pop_source_patches(ctx);
                    let report = check_and_settle(ctx, step_id, &mut lines, check, Refresh::No);
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
                        format!("{stopped}; the edits quilt proves apply were kept")
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
                    return Err(ctx.failed_as(kind, reason));
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
            if stalled {
                break format!(
                    "round {rounds} blocked every remaining patch and changed no file, so \
                     another turn would repeat it"
                );
            }
            match verdict {
                NextRound::Again => {}
                NextRound::Clean => break String::new(),
                NextRound::NoProgress => break format!("round {rounds} made no progress"),
                NextRound::OutOfRounds => break format!("{rounds} round(s) did not fix them"),
            }
        }
    };
    // A patch an agent turn left with an offset or fuzz (fuzz fails rpm's
    // `--fuzz=0`) is refreshed the same way the pre-pass refreshed the rest.
    if report.to_refresh().next().is_some() {
        let offsets = check_and_settle(ctx, step_id, &mut lines, check, Refresh::Offset);
        refreshed.extend(write_refreshed_patches(ctx, &offsets, &mut lines));
    }
    write_refresh_bullets(ctx, step_id, &refreshed);
    for p in &report.patches {
        match &p.status {
            PatchStatus::AppliesWithOffsetOrFuzz { .. } if !refreshed.contains(&p.name) => {
                lines.push(format!(
                    "- {}: applies with an offset or fuzz and quilt could not refresh it; left as it is",
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
    // A patch the agent blocked ends the package too, whatever quilt says.
    if !failing.is_empty() || !blocked.is_empty() {
        for p in &failing {
            lines.push(format!("- {}: still does not apply; kept ({stop})", p.name));
        }
        write_notes(&lines, "quilt");
        step.finish(StepOutcome::Failed);
        return Err(ctx.failed(kept_patches_reason(
            &config.version,
            &stop,
            &failing,
            &blocked,
        )));
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
                "- {}: refreshed by the pipeline (it applied with an offset or fuzz)",
                p.name
            ));
            names.push(p.name.clone());
        }
    }
    names
}

/// Whether the review turn runs for a package nobody asked to patch. A spec
/// generated from a CUDA redistributable is never reviewed: it has no source to
/// read, and the turn can only add a "no test suite" comment.
///
/// `checkout` holds the OBS status and whether OBS built exactly the spec on
/// disk (see `CheckoutFacts::exact_success`). An update that OBS already built
/// needs no edit. An update OBS rejected or failed is reviewed as the carried
/// recipe it is. A package that is not an update is reviewed unless OBS built
/// it with a failing result, which the build rounds handle.
pub(super) fn review_turn_wanted(
    need_spec: bool,
    update: bool,
    checkout: &CheckoutFacts,
    already_reviewed: bool,
    rpm_name: &str,
) -> bool {
    if already_reviewed || crate::redist::is_redist_package(rpm_name) {
        return false;
    }
    if need_spec {
        return true;
    }
    if update {
        return !checkout.exact_success();
    }
    !matches!(
        checkout.obs_status,
        Some(osc::BuildStatus::Failed | osc::BuildStatus::Unresolvable)
    )
}

/// The context of the review turn: a patch request alone, else every note
/// that applies (the empty submodules of a source archive, the carried recipe),
/// so that one note does not hide another.
pub(super) fn review_turn_context(
    patch: Option<&str>,
    submodules: Option<&str>,
    carried: Option<&str>,
) -> Option<String> {
    if let Some(patch) = patch {
        return Some(patch.to_string());
    }
    let notes: Vec<&str> = [submodules, carried].into_iter().flatten().collect();
    (!notes.is_empty()).then(|| notes.join("\n\n"))
}

/// Directories under `<root>/third_party` (or `3rdparty`, `thirdparty`) that
/// are empty: a git submodule a GitHub archive does not contain. A tree with
/// several of them (torch: pybind11, XNNPACK, ...) cannot be built from that
/// archive, and no spec edit changes that.
pub(super) fn empty_submodule_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for parent in ["third_party", "3rdparty", "thirdparty"] {
        let Ok(rd) = std::fs::read_dir(root.join(parent)) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir()
                && std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_none())
                && let Some(name) = e.file_name().to_str()
            {
                out.push(format!("{parent}/{name}"));
            }
        }
    }
    out.sort();
    out
}

/// Review-turn context when the source archive lacks its git submodules, or
/// `None`. Two empty directories are the threshold: one can be a placeholder.
pub(super) fn empty_submodule_context(root: &Path) -> Option<String> {
    let empty = empty_submodule_dirs(root);
    (empty.len() >= 2).then(|| {
        format!(
            "The source archive is missing its git submodules: {} are empty directories. \
             No spec edit supplies them, so do not try: this tree cannot be built from \
             this archive. Return `use_upstream_wheel` now, quoting these directories as the \
             reason, if the project publishes a wheel; otherwise report the dependency that fixes it in `patch_dependencies` or vendor the submodules with a `_service` (never `blocked`).",
            empty.join(", ")
        )
    })
}

/// The `why` the pipeline gives its own wheel switch for an archive without
/// its git submodules.
pub(super) fn empty_submodule_why(root: &Path) -> String {
    format!(
        "the source archive is missing its git submodules ({} are empty directories), \
         so it cannot be built from source",
        empty_submodule_dirs(root).join(", ")
    )
}

/// A `use_upstream_wheel` outcome made by the pipeline itself, so the switch goes
/// through the same path (provenance, notes, state reset) as the agent's.
pub(super) fn pipeline_wheel_outcome(why: &str) -> crate::agent::TurnOutcome {
    crate::agent::TurnOutcome {
        action: crate::agent::Action::UseUpstreamWheel {
            why: why.to_string(),
        },
        reasoning: why.to_string(),
        changelog: String::new(),
        evidence: Vec::new(),
        unresolvable_packages: Vec::new(),
        patch_dependencies: Vec::new(),
        stage_files: Vec::new(),
        patch_answers: Vec::new(),
        confidence: crate::agent::Confidence::High,
        usage: crate::agent::Usage::default(),
        tool_calls: Vec::new(),
        prompt: String::new(),
        output: String::new(),
        raw: String::new(),
        session_id: None,
        ended_early: None,
        denied_commands: Vec::new(),
    }
}

/// A source archive without its git submodules cannot be built by any spec
/// edit: when the wheel exists, switch to it now instead of paying a turn to be
/// told so. `true` when the spec was switched (the Review turn is then not
/// needed); on a failed switch the Review turn decides.
pub(super) fn switch_for_missing_submodules(
    ctx: &RunCtx,
    st: &mut RunState,
    spec_path: &Path,
    lookup: PypiLookup<'_>,
) -> Result<bool, PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let mut switched = false;
    if st.from_repo_archive
        && let root = ws.sdist_dir().join(&st.sdist_top_level)
        && empty_submodule_context(&root).is_some()
    {
        let why = empty_submodule_why(&root);
        let step = begin_or_fail!(
            ctx.pkg_run,
            *reporter,
            StepKind::AgentTurn,
            Some("submodule-wheel"),
            &mut st.step_attempts,
            rpm_name.clone()
        );
        let before = std::fs::read_to_string(spec_path).unwrap_or_default();
        let outcome = pipeline_wheel_outcome(&why);
        let applied = apply_turn_and_reset_with(
            ApplyTurnCtx {
                config,
                checkout_dir,
                spec_path,
                step: &step,
                before: &before,
                rpm_name,
                label: "Review — pre-build (git submodules missing)",
                attempt: 0,
                notes_path: ws.notes(),
                snapshot: None,
                kept: &[],
                staged_out: None,
                rejected_out: None,
            },
            &outcome,
            *reporter,
            &mut st.sdist_top_level,
            &mut st.from_repo_archive,
            lookup,
        );
        match applied {
            Ok(TurnEffect::SwitchedToWheel) => {
                step.finish(ap_core::event::StepOutcome::Ok);
                switched = true;
            }
            other => {
                step.finish(ap_core::event::StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: None,
                    message: format!(
                        "{rpm_name}: the source archive lacks its git submodules and the \
                         wheel switch did not happen ({}); the Review turn decides",
                        other.err().unwrap_or_else(|| "no change".into())
                    ),
                });
            }
        }
    }
    Ok(switched)
}
