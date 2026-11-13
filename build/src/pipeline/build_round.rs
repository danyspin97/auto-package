//! The build round: the build, its rpmlint logs and state, and the post-build
//! review with its revert.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::archive::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;
use crate::pipeline::unresolvable::*;

/// Step: Build (wait for OBS remote build, fetch log, download RPMs; with
/// `--local`, `osc build` on this machine), then
/// route its outcome: success finishes the package, unresolvable dependencies
/// end it with `MissingDeps`, and an ordinary failure runs a fix turn.
pub(super) fn build_round(ctx: &RunCtx, st: &mut RunState) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        rpm_name,
        obs_name,
        ..
    } = ctx;
    let reporter = *reporter;
    let build_step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::Build,
        None,
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let build_log = build_step.file("build.log");

    let build_result = osc::build_package(
        osc::local_build(),
        &config.project,
        obs_name,
        &config.distro,
        &config.arch,
        &ctx.checkout_dir,
        &build_log,
        &config.rpms_dir,
        Emit::new(reporter, build_step.id),
    );

    // A build of a post-build review's edit that did not succeed: the spec that
    // built before is what the package ends with.
    if let Err(e) = &build_result
        && let Some(revert) = st.post_review_revert.take()
    {
        return revert_post_review(ctx, st, build_step, revert, &e.to_string());
    }
    st.post_review_revert = None;

    match build_result {
        Ok(()) => {
            if let Some(end) = post_build_review(ctx, st, &build_log) {
                build_step.finish(StepOutcome::Ok);
                return Ok(end);
            }
            let build_step = build_step.finish(StepOutcome::Ok);
            let record = record_rpmlint_logs(&config.rpms_dir, obs_name, &config.version, &ctx.ws);
            for message in record.warnings {
                reporter.report(&Event::Warning {
                    step: None,
                    message,
                });
            }
            if let Some(end) = rpmlint_fix_round(ctx, st, &build_step, &build_log, &record.errors) {
                return Ok(end);
            }
            Ok(RoundEnd::Finished(build_succeeded(ctx, st, build_step)))
        }
        Err(osc::Error::UnresolvableDeps {
            names: deps,
            detail,
        }) => {
            let build_step = build_step.finish(StepOutcome::Failed);
            unresolvable_deps_round(ctx, st, &build_step, &build_log, deps, detail)
        }
        Err(e @ osc::Error::NotActionable(_)) => {
            build_step.finish(StepOutcome::Failed);
            Err(ctx.failed_osc(&e))
        }
        Err(osc::Error::Excluded(why)) => {
            build_step.finish(StepOutcome::Skipped);
            Err(ctx.failed_as(FailureKind::Excluded, why))
        }
        Err(osc::Error::BuildFailed { .. }) => {
            let build_step = build_step.finish(StepOutcome::Failed);
            fix_after_build(ctx, st, &build_step, &build_log, None)
        }
        // Only a build OBS actually failed is something the agent can fix
        // from its log. A poll that timed out or kept erroring, a launch or
        // I/O failure, or an unreadable log says nothing about the spec, so
        // it ends this attempt instead of being sent to the fix loop.
        Err(e) => {
            build_step.finish(StepOutcome::Failed);
            Err(ctx.failed_osc(&e))
        }
    }
}

/// Append the rpmlint result of the RPMs just downloaded for this package to
/// `state/<pkg>/rpmlint.log`, one dated section per run of rpmlint. The log OBS
/// publishes is overwritten with every build, so this file is the only record
/// of earlier ones. Returns the `E:` lines. For a download that no fix turn
/// follows (the reconcile path), its errors are warnings; a build round reports
/// them itself, see [`rpmlint_fix_round`].
pub(super) fn record_rpmlint(
    config: &PipelineConfig,
    obs_name: &str,
    ws: &Workspace,
    reporter: &dyn Reporter,
) -> Vec<String> {
    let record = record_rpmlint_logs(&config.rpms_dir, obs_name, &config.version, ws);
    for message in record.findings.into_iter().chain(record.warnings) {
        reporter.report(&Event::Warning {
            step: None,
            message,
        });
    }
    record.errors
}

/// What [`record_rpmlint_logs`] found: the findings (one line per log with
/// `E:` lines), the warnings about the record itself, and the rpmlint `E:` lines
/// of the RPMs (already without the rpmlintrc filters OBS applied).
pub(super) struct RpmlintRecord {
    pub(super) findings: Vec<String>,
    pub(super) warnings: Vec<String>,
    pub(super) errors: Vec<String>,
}

/// [`record_rpmlint`] without a reporter: returns the warnings instead. Also
/// used by [`reconcile_with_obs`], so RPMs fetched there are recorded the same
/// way as those of a normal download. `obs_name` is the name the RPMs were
/// downloaded under: the stashed rpmlint logs are keyed by it.
pub(super) fn record_rpmlint_logs(
    rpms_dir: &Path,
    obs_name: &str,
    version: &str,
    ws: &Workspace,
) -> RpmlintRecord {
    let mut findings = Vec::new();
    let mut warnings = Vec::new();
    let mut all_errors = Vec::new();
    let logs = osc::take_rpmlint_logs(rpms_dir, obs_name);
    if logs.is_empty() {
        return RpmlintRecord {
            findings,
            warnings,
            errors: all_errors,
        };
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut text = String::new();
    for (name, log) in &logs {
        text.push_str(&format!(
            "=== rpmlint of {name} {version} (unix time {secs}) ===\n{log}\n"
        ));
        let errors = osc::rpmlint_errors(log);
        if !errors.is_empty() {
            findings.push(rpmlint_finding(name, &errors));
            all_errors.extend(errors);
        }
    }
    let path = ws.dir().join("rpmlint.log");
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
    if let Err(e) = appended {
        warnings.push(format!("could not append to {}: {e}", path.display()));
    }
    RpmlintRecord {
        findings,
        warnings,
        errors: all_errors,
    }
}

/// A successful OBS build with nothing left for the rpmlint round: record it
/// and report the package as built.
fn build_succeeded(ctx: &RunCtx, st: &mut RunState, build_step: StepDir) -> PipelineResult {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        ..
    } = ctx;
    let reporter = *reporter;
    // A build that passes after tests failed in an earlier one: those tests are
    // flaky, and the notes keep that so nobody "fixes" them later.
    if let Some(probe) = crate::flaky::load_probe(ws.dir(), &config.run_id)
        && !probe.compared
    {
        append_notes(
            ws.notes(),
            &format!(
                "\n## flaky tests\nfailed in one build and passed in the next, with no change: {}\n",
                probe.first.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        );
        crate::flaky::clear_probe(ws.dir());
    }
    st.step_attempts
        .skip(reporter, StepKind::ExtractFailure, SkipReason::NoFailure);
    record_built_state(ctx);
    warn_if_err(
        reporter,
        "updating the latest-build link",
        ws.link_latest(&build_step.path),
    );
    // `config.project` now has one more package than whatever
    // this process last cached — see `try_list_project_packages`'s
    // doc comment. Without this, every package built from here
    // on stays invisible to every dependent package's
    // availability check for the rest of the run.
    osc::invalidate_project_packages_cache(&config.project);
    PipelineResult::Built {
        rpm_name: rpm_name.clone(),
    }
}

/// Record in the package's state that its current spec built, on OBS or, under
/// `--local`, on this machine (recorded apart, see [`LOCAL_SUCCESS`]).
fn record_built_state(ctx: &RunCtx) {
    let RunCtx {
        config,
        reporter,
        ws,
        ..
    } = ctx;
    let local = osc::local_build().is_some();
    update_state(ws, *reporter, |state| {
        state.record_built(&config.version, local);
    });
}

/// Most diff lines and warning lines the post-build review is given.
const POST_REVIEW_DIFF_LINES: usize = 200;

pub(super) const POST_REVIEW_WARNING_LINES: usize = 60;

/// The lines of a successful build log worth a second look: compiler and
/// rpmlint warnings, capped, so the agent never reads the log itself.
pub(super) fn build_warnings_extract(log: &str) -> String {
    const MARKERS: [&str; 6] = [
        " W: ",
        " E: ",
        "warning:",
        "Installed (but unpackaged)",
        "Deprecat",
        "rpmlint",
    ];
    log.lines()
        .filter(|l| MARKERS.iter().any(|m| l.contains(m)))
        .take(POST_REVIEW_WARNING_LINES)
        .map(|l| l.chars().take(300).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The least spend room, in dollars, a post-build review turn needs: the review
/// is not started with less left than this, whatever the last turn cost.
pub(super) const POST_REVIEW_MIN_BUDGET_USD: f64 = 0.5;

/// Whether the post-build review is ruled out before any turn: it already ran,
/// the run is an update (its recipe is the origin's, and a clean-up turn would
/// rewrite what the maintainer chose), there is no agent, or the spend guard
/// does not let another turn start (`out_of_budget`, see `may_start_turn`).
pub(super) fn post_review_ruled_out(
    already_done: bool,
    update: bool,
    no_agent: bool,
    out_of_budget: bool,
) -> bool {
    already_done || update || no_agent || out_of_budget
}

/// What the post-build review diffs the spec against: the generated spec, else
/// the spec the run started with (a kept recipe of a `lint` build), else nothing.
pub(super) fn review_baseline(generated: Option<String>, run_start: Option<&str>) -> String {
    generated
        .or_else(|| run_start.map(str::to_string))
        .unwrap_or_default()
}

/// Whether the post-build review has a turn to take: the spec is not empty, and
/// it changed, unless the build is a `lint` review, which takes its turn on an
/// unchanged spec too (a package without findings is still reviewed).
pub(super) fn review_wanted(spec_empty: bool, diff_empty: bool, lint_review: bool) -> bool {
    !spec_empty && (!diff_empty || lint_review)
}

/// One cheap clean-up turn on a spec that has just built, when the agent
/// changed it on the way (a fix attempt or the pre-build review). `Some` is
/// the round to run next: the turn edited the spec, so it is built again, and
/// [`revert_post_review`] puts the built spec back if that build fails. `None`
/// ends the package as built: there was nothing to review, nothing to improve,
/// or the turn could not be used. Runs at most once per run.
fn post_build_review(ctx: &RunCtx, st: &mut RunState, build_log: &Path) -> Option<RoundEnd> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    if post_review_ruled_out(
        std::mem::replace(&mut st.post_review_done, true),
        st.update.is_some(),
        config.no_agent,
        ctx.may_start_turn(st.spent_usd).is_err()
            || spend_allows_turn(config.budget_usd, st.spent_usd, POST_REVIEW_MIN_BUDGET_USD)
                .is_err(),
    ) {
        return None;
    }
    let good = strip_and_rewrite(spec_path);
    let generated = std::fs::read_to_string(ws.dir().join("spec.generated")).ok();
    let baseline = review_baseline(generated, st.run_start_spec.as_deref());
    let diff = unified_diff(&baseline, &good);
    // Nothing the agent changed: the spec is the generated one and was vetted.
    // A `lint` review runs on an unchanged spec too (see `review_wanted`).
    if !review_wanted(
        good.trim().is_empty(),
        diff.trim().is_empty(),
        config.lint_review,
    ) {
        return None;
    }
    let diff: String = if diff.trim().is_empty() {
        "(none)".to_string()
    } else {
        diff.lines()
            .take(POST_REVIEW_DIFF_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let warnings = std::fs::read_to_string(build_log)
        .map(|l| build_warnings_extract(&l))
        .unwrap_or_default();
    let notes = std::fs::read_to_string(ws.notes()).unwrap_or_default();
    let journal: String = notes
        .chars()
        .rev()
        .take(4096)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let context = format!(
        "Changes the earlier turns made to the spec (`-` removed, `+` added), against the \
         generated spec or, for a kept recipe, the spec the run started with; `(none)` when \
         nothing changed:\n{}\n\
         Warnings of the build (a capped extract; the log itself is not needed):\n{}\n\
         End of the attempt journal:\n{}",
        crate::agent::fenced_data(&diff),
        crate::agent::fenced_data(if warnings.is_empty() {
            "(none)"
        } else {
            &warnings
        }),
        crate::agent::fenced_data(&journal),
    );

    let snapshot = CheckoutSnapshot::take(checkout_dir);
    let turn = agent_turn(
        TurnCtx {
            carried: false,
            label: "post-build-review",
            task: crate::agent::Task::PostBuildReview,
            event_task: ap_core::event::Task::PostBuildReview,
            // The cheapest tier: a wrong edit costs a rebuild, then a revert.
            attempt: 1,
            hint: None,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: false,
            resolver: false,
            context: Some(&context),
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    );
    let (agent_step, outcome) = match turn {
        Ok(v) => v,
        Err((e, kind)) => {
            discard_failed_turn(
                ws.notes(),
                reporter,
                &snapshot,
                spec_path,
                &good,
                "PostBuildReview",
            );
            if ends_package(kind) {
                return Some(RoundEnd::Finished(ctx.failed_as(kind, e)));
            }
            reporter.report(&Event::Warning {
                step: None,
                message: format!("{rpm_name}: post-build review skipped: {e}"),
            });
            return None;
        }
    };
    let mut staged: Vec<String> = Vec::new();
    let effect = apply_turn_and_reset(
        ApplyTurnCtx {
            config,
            checkout_dir,
            spec_path,
            step: &agent_step,
            before: &good,
            rpm_name,
            label: "PostBuildReview",
            attempt: st.attempts,
            notes_path: ws.notes(),
            snapshot: Some(&snapshot),
            kept: &[],
            staged_out: Some(&mut staged),
            rejected_out: None,
        },
        &outcome,
        reporter,
        &mut st.sdist_top_level,
        &mut st.from_repo_archive,
    );
    // A patch the clean-up wrote or edited is reviewed like any other.
    let effect = match effect {
        Ok(e) => {
            match review_turn_patches(ctx, st, &agent_step, &snapshot, &good, "PostBuildReview", e)
            {
                Ok(e) => Ok(e),
                Err(result) => return Some(RoundEnd::Finished(result)),
            }
        }
        other => other,
    };
    // An edit of the comments alone would be built again for nothing: it is
    // discarded and the spec that built stands, with no rebuild.
    if matches!(effect, Ok(TurnEffect::Edited)) && is_comment_only_edit(&snapshot, spec_path, &good)
    {
        discard_comment_only_edit(
            &snapshot,
            spec_path,
            &good,
            Some(&agent_step),
            ws.notes(),
            REVIEW_COMMENT_ONLY,
            reporter,
        );
        return None;
    }
    match &effect {
        Ok(TurnEffect::Edited) => {
            st.post_review_revert = Some(PostReviewRevert {
                spec: good,
                snapshot,
                kind: RevertKind::PostReview,
            });
            Some(RoundEnd::Retry)
        }
        // No change, a refused edit (already rolled back by the gate), or an
        // effect the review has no business with: the built spec stands. A
        // wheel switch refused for a security patch is one of them: the build is
        // green, so the package keeps it and the refusal is only a warning.
        Ok(_) | Err(_) => {
            if let Err(e) = &effect {
                reporter.report(&Event::Progress {
                    step: Some(agent_step.id),
                    message: format!("{rpm_name}: post-build review turn not applied: {e}"),
                });
            }
            snapshot.restore(reporter, Some(&agent_step));
            restore_spec(spec_path, &good, Some(&agent_step), reporter);
            None
        }
    }
}

/// The warning that says an edit made after a green build was reverted, for
/// the build of that edit failing with `why`.
pub(super) fn revert_warning(rpm_name: &str, kind: &RevertKind, why: &str) -> String {
    match kind {
        RevertKind::PostReview => format!(
            "{rpm_name}: the build after the post-build review failed ({}); reverting to the spec \
             that built",
            brief(why)
        ),
        RevertKind::Rpmlint(errors) => rpmlint_warning(
            rpm_name,
            &format!(
                "the build after the rpmlint fix failed ({}); reverted to the spec that built",
                brief(why)
            ),
            errors,
        ),
    }
}

/// The build after an edit made after a green build failed: put back the spec
/// (and checkout files) that built, publish it again, and end the package as
/// built. The RPMs of that build are still in the RPM directory.
fn revert_post_review(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: StepGuard<'_>,
    revert: PostReviewRevert,
    why: &str,
) -> Result<RoundEnd, PipelineResult> {
    let reporter = ctx.reporter;
    let build_step = build_step.finish(StepOutcome::Failed);
    let message = revert_warning(&ctx.rpm_name, &revert.kind, why);
    // A reverted post-build review edit leaves the package as it was built, which
    // is not a problem. A reverted rpmlint fix ships with its errors: a warning.
    let step = Some(build_step.id);
    reporter.report(&match revert.kind {
        RevertKind::PostReview => Event::Progress { step, message },
        RevertKind::Rpmlint(_) => Event::Warning { step, message },
    });
    revert.snapshot.restore(reporter, Some(&build_step));
    restore_spec(&ctx.spec_path, &revert.spec, Some(&build_step), reporter);
    let note = match &revert.kind {
        RevertKind::PostReview => {
            "\n## Post-build review reverted\nThe build after the review's edit failed; the spec that built was restored.\n"
        }
        RevertKind::Rpmlint(_) => {
            "\n## rpmlint fix reverted\nThe build after the rpmlint fix turn's edit failed; the spec that built was restored.\n"
        }
    };
    append_notes(ctx.ws.notes(), note);
    commit_unless_local(ctx, st)?;
    record_built_state(ctx);
    osc::invalidate_project_packages_cache(&ctx.config.project);
    Ok(RoundEnd::Finished(PipelineResult::Built {
        rpm_name: ctx.rpm_name.clone(),
    }))
}
