//! The fixes that follow a failed validation or build, and the flaky probe they save.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::archive::*;
use crate::pipeline::classify::*;
use crate::pipeline::ladder::*;
use crate::pipeline::logs::*;
use crate::pipeline::network_fetch::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::repeat_log::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// One fix-after-validation agent turn: counts the attempt, runs the turn and
/// applies its outcome. `apply_label` names the apply step and
/// `agent_failed_prefix` starts the error text when the turn itself fails.
pub(super) fn fix_after_validation(
    ctx: &RunCtx,
    st: &mut RunState,
    context: Option<&str>,
    apply_label: &str,
    agent_failed_prefix: &str,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    // Never commit a spec that is not sound — OBS would spend a whole remote
    // build to tell us what the local check already said.
    st.attempts += 1;
    persist_attempts(ws, &config.run_id, st.attempts);
    let attempts = st.attempts;
    let clean = strip_and_rewrite(spec_path);
    let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);
    let turn = agent_turn(
        TurnCtx {
            carried: false,
            label: "fix-after-validation",
            task: crate::agent::Task::FixAfterValidation,
            event_task: ap_core::event::Task::FixAfterBuild {
                attempt: attempts as u32,
                max_attempts: config.max_attempts as u32,
            },
            attempt: attempts,
            hint: None,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: true,
            context,
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    );

    match turn {
        Ok((agent_step, outcome)) => {
            let mut staged: Vec<String> = Vec::new();
            let effect = match apply_turn_and_reset(
                ApplyTurnCtx {
                    config,
                    checkout_dir,
                    spec_path,
                    step: &agent_step,
                    before: &clean,
                    rpm_name,
                    label: apply_label,
                    attempt: attempts,
                    notes_path: ws.notes(),
                    snapshot: Some(&checkout_snapshot),
                    kept: &[],
                    staged_out: Some(&mut staged),
                    rejected_out: None,
                },
                &outcome,
                *reporter,
                &mut st.sdist_top_level,
                &mut st.from_repo_archive,
            ) {
                Ok(effect) => effect,
                Err(reason) => resolve_blocked(
                    ctx,
                    st,
                    LadderSite {
                        build_log: None,
                        staged: &staged,
                        task: crate::agent::Task::FixAfterValidation,
                        event_task: ap_core::event::Task::FixAfterBuild {
                            attempt: attempts as u32,
                            max_attempts: config.max_attempts as u32,
                        },
                        attempt: attempts,
                        hint: None,
                        context,
                        apply_label,
                        before: &clean,
                        snapshot: &checkout_snapshot,
                        step: &agent_step,
                    },
                    &outcome,
                    reason,
                )?,
            };
            if effect == TurnEffect::Edited
                && let Some(missing) = missing_deps_added_by_edit(ctx, agent_step.id, &clean)
            {
                return Err(missing);
            }
            Ok(())
        }
        Err((e, kind)) => {
            discard_failed_turn(
                ws.notes(),
                *reporter,
                &checkout_snapshot,
                spec_path,
                &clean,
                "FixAfterValidation",
            );
            Err(ctx.failed_as(kind, format!("{agent_failed_prefix}{e}")))
        }
    }
}

/// An ordinary build failure: check the attempt/budget limits, then run a
/// fix-after-build turn and apply it.
/// Save the first flaky-test probe; false (with a warning) when it could not be
/// written, so the caller does not start a rebuild nobody could compare.
fn save_flaky_probe(
    dir: &Path,
    probe: &crate::flaky::Probe,
    reporter: &dyn Reporter,
    step: ap_core::event::StepId,
) -> bool {
    match crate::flaky::save_probe(dir, probe) {
        Ok(()) => true,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(step),
                message: format!("could not save the flaky-test probe, so no extra build: {e}"),
            });
            false
        }
    }
}

pub(super) fn fix_after_build(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: &StepDir,
    build_log: &Path,
    context: Option<&str>,
) -> Result<RoundEnd, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        obs_name,
        ..
    } = ctx;
    let reporter = *reporter;

    if config.no_agent {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            st.attempts,
            st.spent_usd,
        );
        return Err(ctx.failed(format!("build failed; log: {}", build_log.display())));
    }

    // A build that fetches its dependencies fails the same way on every try, and
    // no edit of the spec changes that: the package ends here, with no fix turn
    // and no rebuild.
    let log_text = std::fs::read_to_string(build_log).unwrap_or_default();
    if let Some(evidence) = network_fetch_failure(&log_text) {
        return Err(ctx.failed(format!(
            "{NETWORK_FETCH_REASON}: {evidence}; log: {}",
            build_log.display()
        )));
    }

    // The agent can only act on what the log says. The same log twice is OBS
    // handing back the build it already had (nothing new reached it, or its
    // verdict was read before the rebuild started), so a turn on it would only
    // repeat the last one: python-chromadb and python-torchcodec spent five to
    // ten turns on one stale log. A rebuild is asked for once; the same log
    // again ends the package with the reason.
    // Only tests failed: one more build tells a flaky test from a stable failure
    // before the agent is asked about it (convention 87).
    let mut flaky_context: Option<String> = None;
    if !ap_core::env::env_flag("AUTO_PACKAGE_NO_FLAKY_RETRY") {
        let log_text = std::fs::read_to_string(build_log).unwrap_or_default();
        let ids = crate::flaky::failing_ids(&log_text);
        match (crate::flaky::load_probe(ws.dir(), &config.run_id), ids) {
            // The guard saves the probe; when that fails there is no retry, or
            // every round would ask for another rebuild.
            (None, Some(ids))
                if save_flaky_probe(
                    ws.dir(),
                    &crate::flaky::Probe {
                        run_id: config.run_id.clone(),
                        first: ids.clone(),
                        compared: false,
                    },
                    reporter,
                    build_step.id,
                ) =>
            {
                let message = match flaky_retry(osc::local_build().is_some()) {
                    // Under `--local` the retry is the next round's local build:
                    // nothing is asked of OBS.
                    FlakyRetry::LocalBuild => format!(
                        "only {} test(s) failed: running one more local build of the same \
                         sources to tell flaky tests from stable failures",
                        ids.len()
                    ),
                    FlakyRetry::ObsRebuild => {
                        match osc::rebuild(
                            &config.project,
                            obs_name,
                            &config.distro,
                            &config.arch,
                            Emit::new(reporter, build_step.id),
                        ) {
                            Ok(_) => format!(
                                "only {} test(s) failed: asked OBS for one more build of the \
                                 same sources to tell flaky tests from stable failures",
                                ids.len()
                            ),
                            Err(e) => format!(
                                "only {} test(s) failed; the rebuild that would tell whether \
                                 they are flaky could not be requested: {e}",
                                ids.len()
                            ),
                        }
                    }
                };
                reporter.report(&Event::Warning {
                    step: Some(build_step.id),
                    message,
                });
                return Ok(RoundEnd::Retry);
            }
            (Some(mut probe), second) if !probe.compared => {
                probe.compared = true;
                let _ = crate::flaky::save_probe(ws.dir(), &probe);
                if let Some(second) = second
                    && let Some(verdict) = crate::flaky::compare(&probe.first, &second)
                {
                    append_notes(
                        ws.notes(),
                        &format!("\n## flaky tests\n{}\n", verdict.note()),
                    );
                    flaky_context = Some(verdict.context());
                }
            }
            _ => {}
        }
    }
    let digest = sha256_file(build_log).ok();
    let previous = std::mem::replace(&mut st.last_log_digest, digest.clone());
    let mut fresh_note: Option<String> = None;
    if digest.is_some() && previous == digest {
        st.identical_logs += 1;
        let local = osc::local_build().is_some();
        match same_log_action(local, st.identical_logs) {
            SameLogAction::Fail if local => {
                return Err(ctx.failed(format!(
                    "the local build gave the same log {} times for the same sources, so the \
                     changes made since did not change its result; log: {}",
                    st.identical_logs + 1,
                    build_log.display()
                )));
            }
            SameLogAction::Fail => {
                return Err(ctx.failed(format!(
                    "OBS returned the same build log {} times for the same sources even after a \
                     rebuild was requested; nothing the agent can change explains it; log: {}",
                    st.identical_logs + 1,
                    build_log.display()
                )));
            }
            // Under `--local` this log is the build that just ran, not a copy
            // of an old one OBS may have handed back: nothing to ask for. The
            // agent reads it as the result of its last change.
            SameLogAction::Continue => {
                fresh_note = Some(
                    "This build.log is identical to the one the previous attempt saw: the local \
                     build ran again on the same sources and failed the same way, so the last \
                     change did not alter this failure."
                        .to_string(),
                );
            }
            SameLogAction::AskObsRebuild => {
                let emit = Emit::new(reporter, build_step.id);
                let started = osc::rebuild(
                    &config.project,
                    obs_name,
                    &config.distro,
                    &config.arch,
                    emit,
                );
                reporter.report(&Event::Warning {
                    step: Some(build_step.id),
                    message: match &started {
                        Ok(true) => "the build log is identical to the last attempt's: asked OBS for a rebuild".to_string(),
                        Ok(false) => "the build log is identical to the last attempt's: asked OBS for a rebuild, \
                                      but it had not started after 90 s".to_string(),
                        Err(e) => format!("the build log is identical to the last attempt's, and the rebuild request failed: {e}"),
                    },
                });
                st.attempts += 1;
                persist_attempts(ws, &config.run_id, st.attempts);
                if st.attempts > config.max_attempts {
                    return Err(ctx.failed(format!(
                        "attempt limit {} reached while OBS kept returning the same build log; log: {}",
                        config.max_attempts,
                        build_log.display()
                    )));
                }
                return Ok(RoundEnd::Retry);
            }
        }
    } else if digest.is_some() && previous.is_some() {
        st.identical_logs = 0;
        // Said as a fact, because the turns that called a new log stale were
        // reading timestamps, not comparing it with the last one.
        fresh_note = Some(
            "This build.log is NOT the one the previous attempt saw: it differs from it, so a new \
             build ran against the current sources. Do not call it stale; read it as the current \
             result of your last change."
                .to_string(),
        );
    }
    if let Some(fc) = flaky_context {
        fresh_note = Some(match fresh_note {
            Some(f) => format!("{f}\n\n{fc}"),
            None => fc,
        });
    }
    let context_owned: Option<String> = match (context, fresh_note) {
        (Some(c), Some(f)) => Some(format!("{c}\n\n{f}")),
        (Some(c), None) => Some(c.to_string()),
        (None, f) => f,
    };
    let context = context_owned.as_deref();

    st.attempts += 1;
    persist_attempts(ws, &config.run_id, st.attempts);
    let attempts = st.attempts;
    if attempts > config.max_attempts || st.spent_usd >= config.budget_usd {
        skip_agent(
            reporter,
            &mut st.step_attempts,
            config,
            attempts,
            st.spent_usd,
        );
        let why = if st.spent_usd >= config.budget_usd {
            format!(
                "budget exhausted (${:.2} spent of ${:.2})",
                st.spent_usd, config.budget_usd
            )
        } else {
            format!("attempt limit {} reached", config.max_attempts)
        };
        return Err(ctx.failed_as(
            FailureKind::Exhausted,
            format!(
                "gave up after {} attempt(s): {}; log: {}",
                attempts,
                why,
                build_log.display()
            ),
        ));
    }

    link_build_log(ws, build_log);

    let extract = extract_failure_step(
        pkg_run,
        reporter,
        &mut st.step_attempts,
        build_log,
        &st.sdist_top_level,
        build_step.id,
    );
    reporter.report(&Event::BuildFailed {
        step: build_step.id,
        classification: classify_failure(&extract),
        log: build_log.to_path_buf(),
    });

    // A failure that comes back in the same shape with only a name changed
    // (pcp: four builds in a row, each `cd` into one more PMDA directory 7.2.2
    // no longer installs) is a family of defects fixed one per build.
    let shape = failure_shape(&extract);
    if shape.is_some() && shape == st.last_failure_shape {
        st.same_shape_failures += 1;
    } else {
        st.same_shape_failures = 0;
    }
    st.last_failure_shape = shape;
    let context_owned: Option<String> = match (context, same_shape_note(st.same_shape_failures)) {
        (Some(c), Some(n)) => Some(format!("{c}\n\n{n}")),
        (Some(c), None) => Some(c.to_string()),
        (None, n) => n,
    };
    let context = context_owned.as_deref();

    fix_turn(
        ctx,
        st,
        attempts,
        crate::agent::Task::FixAfterBuild,
        Some(&extract),
        context,
        Some(build_log),
    )?;
    Ok(RoundEnd::Retry)
}

/// The shape of a build failure: its phase and last error line with every path
/// cut to its directory, digits and temp-file names blurred. Two failures with
/// the same shape differ only in a name (which directory, which file).
pub(super) fn failure_shape(extract: &crate::log::ExtractedLog) -> Option<String> {
    let phase = extract.failed_section.clone().unwrap_or_default();
    let line = extract
        .excerpt
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with('+'))
        .or_else(|| extract.error_lines.first().map(String::as_str))?;
    let words: Vec<String> = line
        .split_whitespace()
        .map(|w| {
            let w = w.trim_end_matches(':');
            let w = match w.rfind('/') {
                Some(i) if i > 0 => &w[..i],
                _ => w,
            };
            let mut out = String::with_capacity(w.len());
            let mut digits = false;
            for c in w.chars() {
                if c.is_ascii_digit() {
                    if !digits {
                        out.push('#');
                    }
                    digits = true;
                } else {
                    digits = false;
                    out.push(c);
                }
            }
            out
        })
        .collect();
    Some(format!("{phase}|{}", words.join(" ")))
}

/// What the fix turn is told after `repeats` consecutive builds failed in the
/// same shape (see [`failure_shape`]); `None` for the first or second one.
pub(super) fn same_shape_note(repeats: usize) -> Option<String> {
    (repeats >= 1).then(|| {
        format!(
            "This is build failure number {} in a row of the same kind: only the name in the \
             error changed (a different directory, file or test), so each fix repaired one \
             instance and the next build found another. Do not fix just the one in this log: \
             find every other instance of the same pattern in the spec now (every loop entry, \
             subpackage, `%files` or list line of that kind), check each against the unpacked \
             source and against what this build installed, and fix them all in this one edit.",
            repeats + 1
        )
    })
}
