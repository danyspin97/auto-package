//! The patch loop: the patch stage with its turns, and the empty submodule and
//! wheel outcome handling.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

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
    // A patch quilt proves already applied is removed now: certainly right,
    // and nothing for a turn to do. The rest of the report is what is left.
    let mut check_and_settle = |ctx: &RunCtx, lines: &mut Vec<String>| {
        let mut report = check(ctx, Refresh::No);
        let proven: Vec<String> = report.already_applied().map(|p| p.name.clone()).collect();
        lines.extend(remove_proven_patches(ctx, step_id, &proven));
        report
            .patches
            .retain(|p| !matches!(p.status, PatchStatus::AlreadyApplied { .. }));
        report
    };
    let mut report = check_and_settle(ctx, &mut lines);
    if report.patches.is_empty() || report.not_run.is_some() || report.is_clean() {
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
    let hints = stale_patches(
        &std::fs::read_to_string(spec_path).unwrap_or_default(),
        checkout_dir,
        &st.sdist_top_level,
        &config.version,
    );
    let mut refreshed: std::collections::BTreeSet<String> = Default::default();
    let mut rounds = 0usize;
    // What the last round must hear: why its edit was refused.
    let mut feedback: Option<String> = None;
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
            write_patch_series(&before_spec, checkout_dir);
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
                    discard_failed_turn(
                        ws.notes(),
                        reporter,
                        &snapshot,
                        spec_path,
                        &before_spec,
                        "RefreshPatches",
                    );
                    pop_source_patches(ctx);
                    // The rounds before this one keep their refreshed patches.
                    write_refresh_bullets(ctx, step_id, &refreshed);
                    write_notes(&lines, "quilt, stopped");
                    step.finish(StepOutcome::Failed);
                    return Err(ctx.failed_as(kind, format!("the agent turn failed: {e}")));
                }
            };
            let applied =
                apply_patch_turn(ctx, &turn_step, &outcome, &before_spec, &snapshot, rounds);
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
            let next = check_and_settle(ctx, &mut lines);
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
            match verdict {
                NextRound::Again => {}
                NextRound::Clean => break String::new(),
                NextRound::NoProgress => break format!("round {rounds} made no progress"),
                NextRound::OutOfRounds => break format!("{rounds} round(s) did not fix them"),
            }
        }
    };
    // A patch that applies only with fuzz fails rpm's `--fuzz=0`; refreshing it is
    // mechanical, so the pipeline does it when no turn did.
    let fuzzy = report.to_refresh().any(|p| {
        matches!(
            p.status,
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }
        )
    });
    if fuzzy {
        for p in check(ctx, Refresh::Fuzzy).patches {
            if let Some(bytes) = &p.refreshed {
                write_or_warn(checkout_dir.join(&p.name), bytes);
                refreshed.insert(p.name.clone());
                lines.push(format!(
                    "- {}: refreshed by the pipeline (it applied only with fuzz)",
                    p.name
                ));
            }
        }
    }
    write_refresh_bullets(ctx, step_id, &refreshed);
    for p in &report.patches {
        match &p.status {
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. } => lines.push(format!(
                "- {}: applies with an offset; left as it is",
                p.name
            )),
            PatchStatus::NotChecked { reason } => {
                lines.push(format!("- {}: not checked: {reason}", p.name))
            }
            _ => {}
        }
    }
    // What still fails is kept, never dropped: the package stops for a human.
    let failing: Vec<&crate::patch_check::PatchResult> = report.failing().collect();
    if !failing.is_empty() {
        for p in &failing {
            lines.push(format!("- {}: still does not apply; kept ({stop})", p.name));
        }
        write_notes(&lines, "quilt");
        step.finish(StepOutcome::Failed);
        return Err(ctx.failed(kept_patches_reason(&config.version, &stop, &failing)));
    }
    write_notes(&lines, "quilt");
    step.finish(StepOutcome::Ok);
    Ok(())
}

/// Whether the review turn runs for a package nobody asked to patch. A spec
/// generated from a CUDA redistributable is never reviewed: it has no source to
/// read, and the turn can only add a "no test suite" comment.
pub(super) fn review_turn_wanted(
    need_spec: bool,
    build_log_nonempty: bool,
    already_reviewed: bool,
    rpm_name: &str,
) -> bool {
    (need_spec || !build_log_nonempty)
        && !already_reviewed
        && !crate::redist::is_redist_package(rpm_name)
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
