//! The agent turn: the turn itself, and the error and skip handling around it.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::licence::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::sibling::*;
use crate::pipeline::spec_edits::*;
use crate::pipeline::turn_spend::*;

/// The kind a failed agent turn is recorded as. A rejected change to the
/// checkout's metadata is `Tampered`, which no caller downgrades or retries.
pub(super) fn turn_failure_kind(e: &crate::agent::Error) -> FailureKind {
    if e.is_not_logged_in() {
        FailureKind::AgentUnavailable
    } else if e.is_rate_limited() {
        FailureKind::RateLimited
    } else if matches!(e, crate::agent::Error::Tampered { .. }) {
        FailureKind::Tampered
    } else {
        // The turn itself failed (a crash, a timeout, an answer that could not
        // be read): not the edit the gate refuses.
        FailureKind::AgentFailed
    }
}

/// Whether a failed turn ends the package whatever its caller does: the turn
/// changed the checkout's metadata (`Tampered`) or the package's `Version:`
/// (`VersionChanged`). No caller downgrades, skips or retries these.
pub(super) fn ends_package(kind: FailureKind) -> bool {
    matches!(kind, FailureKind::Tampered | FailureKind::VersionChanged)
}

/// The shared run environment an agent turn works in.
pub(super) struct TurnEnv<'a> {
    pub(super) config: &'a PipelineConfig,
    pub(super) ws: &'a Workspace,
    pub(super) pkg_run: &'a PkgRunDir,
    pub(super) reporter: &'a dyn Reporter,
    /// The package's OBS name, for reading its checkout's spec.
    pub(super) obs_name: &'a str,
    /// The package's turn signals (see [`TurnSignals`]).
    pub(super) signals: std::sync::Arc<std::sync::Mutex<TurnSignals>>,
    /// The patch files a PatchReview turn may write; empty for every other task.
    pub(super) writable_patches: &'a [String],
}

/// What one agent turn needs beyond [`PipelineConfig`].
pub(super) struct TurnCtx<'a> {
    /// Names the turn: the step directory is `NN-agent-<label>` and the label
    /// is the `StepStarted` detail, e.g. `"review"` or `"fix-after-build"`.
    pub(super) label: &'a str,
    pub(super) task: crate::agent::Task,
    pub(super) event_task: ap_core::event::Task,
    pub(super) attempt: usize,
    pub(super) hint: Option<&'a crate::log::ExtractedLog>,
    pub(super) unresolvable_deps: &'a [String],
    pub(super) unresolvable_detail: &'a [String],
    pub(super) checkout_dir: Option<&'a Path>,
    pub(super) from_repo_archive: bool,
    /// Resume the previous `claude` session rather than starting a fresh one.
    pub(super) resume: bool,
    /// Optional short note injected into the prompt before the hint section.
    pub(super) context: Option<&'a str>,
    /// The spec is a carried-forward recipe (see `TurnRequest::carried`).
    pub(super) carried: bool,
}

/// Sent in the fresh session after a turn that ended without calling
/// StructuredOutput. Any edit it made is already on disk, so it must not redo
/// the work, and a prose description of an edit is not accepted as a response.
pub(super) const STRUCTURED_OUTPUT_NUDGE: &str = "Your previous reply did not call the \
StructuredOutput tool, so only your final response was lost: any edits you \
already made to the spec, patches or checkout are on disk and are kept. Do NOT \
redo that work. Look at the current files to confirm what is there, then call \
StructuredOutput with your result (use action `edited` if you changed anything). \
Describing an edit in prose is not accepted; only the StructuredOutput call is.";

/// Reshape an agent turn's `Error` into the one-line reason the pipeline
/// propagates to the user via `PipelineResult::Failed`.
///
/// The default `Display` for `Error::Failed`/`Error::EndedEarly` is useful
/// for debugging (full stderr dump) but doesn't tell the user how to fix
/// the common cases. For example, `claude exited 1` with empty stderr can mean
/// `--max-budget-usd` ran out mid-turn, and nothing in the opaque message points
/// at the one knob (`--budget-usd`) to raise. The
/// claude CLI DOES emit the diagnosis in the stream (`subtype:
/// "error_max_budget_usd"`, `errors: ["Reached maximum budget (...)"]`)
/// and `Error::EndedEarly` carries both; translate them into something
/// the user can act on.
///
/// Pull the inner CLI message out via `ended_early_message()` rather than
/// embedding `{e}`'s `Display` — `Display` prepends
/// `"claude ended turn with <subtype>: "` AND repeats the subtype, so
/// wrapping it in the budget sentence would produce
/// `"model budget of $2.00 (--budget-usd) exhausted mid-turn (claude
/// ended turn with error_max_budget_usd: Reached maximum budget ($0.03))"`
/// — two different dollar figures with no indication that $2.00 is the
/// per-package cap and $0.03 is the per-turn remainder.
pub(super) fn friendly_agent_error(
    e: &crate::agent::Error,
    budget_usd: f64,
    pkg_name: &str,
    max_turns: u32,
) -> String {
    if e.is_not_logged_in() {
        return crate::agent::NOT_LOGGED_IN_MESSAGE.to_string();
    }
    if e.is_budget_exhausted() {
        let detail = e.ended_early_message().unwrap_or("budget exhausted");
        return format!(
            "model budget of ${budget_usd:.2} (--budget-usd) exhausted \
             mid-turn: {detail}. Raise --budget-usd for this package \
             and retry. The agent's prompt + partial output are \
             preserved in the step dir (crash-prompt.md, \
             crash-stdout.log) if you need to see how far it got."
        );
    }
    if e.is_max_turns() {
        let used = match e {
            crate::agent::Error::EndedEarly(x) => x.turns,
            _ => 0,
        };
        return format!(
            "the agent used {used} of its {max_turns} turns (--max-turns) before it answered \
             for {pkg_name}. A turn that runs out of turns is never retried, so the package \
             stops here; the prompt + partial output are preserved in the step dir \
             (crash-prompt.md, crash-stdout.log)."
        );
    }
    if e.is_rate_limited() {
        let detail = e.ended_early_message().unwrap_or("rate limited");
        return format!(
            "the model API rejected the turn with a quota/rate limit (429): \
             {detail}\nThis is a provider-side limit, not a package problem: \
             wait a minute and re-run (state is kept), lower --jobs, or raise \
             the quota. Partial output is in the step dir (crash-stdout.log)."
        );
    }
    if let (Some(subtype), Some(message)) = (e.ended_early_subtype(), e.ended_early_message()) {
        return format!(
            "claude ended turn early ({subtype}): {message}. The \
             agent's prompt + partial output are preserved in the \
             step dir (crash-prompt.md, crash-stdout.log)."
        );
    }
    format!("agent error for {pkg_name}: {e}")
}

/// Report why no further agent turn will run for this package: the same check
/// [`can_start`] makes before a turn.
pub(super) fn skip_agent(
    reporter: &dyn Reporter,
    step_attempts: &mut Attempts,
    config: &PipelineConfig,
    attempts: usize,
    spent_usd: f64,
    signals: &std::sync::Mutex<TurnSignals>,
) {
    let reason = if config.no_agent {
        SkipReason::NoAgent
    } else if let Err(why) = can_start(config.budget_usd, spent_usd, signals) {
        if why.starts_with(BUDGET_REACHED) {
            SkipReason::BudgetExhausted
        } else {
            SkipReason::Other(why)
        }
    } else {
        SkipReason::Other(format!(
            "attempt limit reached ({attempts} of {})",
            config.max_attempts
        ))
    };
    step_attempts.skip(reporter, StepKind::AgentTurn, reason);
}

/// Run one agent turn and emit every event it produces.
///
/// Allocates the step directory, writes `prompt.md`, `output.md`,
/// `response.json` and — when a hint was supplied — `extract.json`, reports
/// `AgentPrompt`/`ExtractResult`/`AgentToolUse`/`AgentReply`/`StepFinished`,
/// and folds the turn's cost into `spent_usd`. On failure the step is reported
/// failed and the error returned; the caller decides whether that is fatal.
pub(super) fn agent_turn(
    ctx: TurnCtx<'_>,
    env: TurnEnv<'_>,
    spent_usd: &mut f64,
    session_id: &mut Option<String>,
    step_attempts: &mut Attempts,
) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
    let TurnEnv {
        config,
        ws,
        pkg_run,
        reporter,
        obs_name,
        signals,
        writable_patches,
    } = env;
    // No turn starts past the budget or after a repeated `no change` (see `can_start`).
    if let Err(why) = can_start(config.budget_usd, *spent_usd, &signals) {
        return Err((why, FailureKind::Exhausted));
    }
    // The spec's Version is the package's before every turn, and a turn may not
    // change it (see `version_stop`): a change made between turns stops here.
    let version_spec = ctx.checkout_dir.map(|d| d.join(format!("{obs_name}.spec")));
    // The patches as they are before the turn are recorded, and settled when the
    // turn ends (see `TurnRecord`); a turn that cannot be recorded does not run.
    let _record = match version_spec
        .as_deref()
        .filter(|_| ctx.task != crate::agent::Task::PatchReview)
        .map(|p| TurnRecord::begin(ws, p))
        .transpose()
    {
        Ok(record) => record,
        Err(why) => return Err((why, FailureKind::NeedsMaintainer)),
    };
    let before_spec = version_spec
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(why) = version_check(
        version_spec.as_deref(),
        &config.version,
        &format!("{}, before the turn", ctx.label),
    ) {
        return Err((why, FailureKind::VersionChanged));
    }
    let step = StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::AgentTurn,
        Some(ctx.label),
        step_attempts,
    )
    .map_err(|e| (e.to_string(), FailureKind::Other))?;

    if let Err(message) = daily_cap_check(&config.work_dir) {
        let step = step.finish(StepOutcome::Failed);
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: message.clone(),
        });
        return Err((message, FailureKind::Exhausted));
    }

    if let Some(hint) = ctx.hint {
        let extract = hint.to_event_extract();
        let artifact = step.file("extract.json");
        if let Ok(json) = serde_json::to_string_pretty(&extract) {
            write_or_warn(&artifact, json);
        }
        reporter.report(&Event::ExtractResult {
            step: step.id,
            extract,
            artifact,
        });
    }

    let notes = tail_for_prompt(&std::fs::read_to_string(ws.notes()).unwrap_or_default());
    let package_notes = package_notes_for_prompt(&crate::package_notes::read(&config.rpm_name));
    // What convention 78 asks a turn to check, read here: the agent's own tools
    // cannot fetch another project's recipe (`osc cat`, `zypper info` are refused),
    // so torchcodec's turns could only answer `blocked`.
    let licence_facts = ctx
        .checkout_dir
        .and_then(|dir| licence_guard_facts(config, obs_name, &ws.build_log(), dir));
    // The packages this one depends on, and the ones a blocker names: their
    // checkouts are readable in the turn and their file lists are inlined.
    let spec_text = ctx
        .checkout_dir
        .map(|d| d.join(format!("{obs_name}.spec")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let sibling_dirs = sibling_checkouts(config, &spec_text);
    let sibling_text = sibling_facts(config, &sibling_dirs);
    let facts: Option<String> = match (licence_facts, sibling_text) {
        (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
        (a, b) => a.or(b),
    };
    let base_context: Option<String> = match (ctx.context, facts) {
        (Some(c), Some(f)) => Some(format!("{c}\n\n{f}")),
        (Some(c), None) => Some(c.to_string()),
        (None, f) => f,
    };
    let on_tool_use = |tool: &str, input: &str| {
        reporter.report(&Event::AgentToolUse {
            step: step.id,
            tool: tool.to_string(),
            input: input.to_string(),
        });
    };
    // After a prose-only reply the retry runs in a fresh session with this
    // nudge added to the prompt context (see below).
    let fresh_session = std::cell::Cell::new(false);
    // Moved along after each 429 so the retry runs on a different model (each
    // model has its own quota).
    let model_rotation = std::cell::Cell::new(0usize);
    let nudge: std::cell::OnceCell<String> = std::cell::OnceCell::new();
    // `spent` is what the package has spent when this call starts; it is passed
    // in so the closure does not hold `spent_usd` while the loop charges it.
    let make_req = |spent: f64| crate::agent::TurnRequest {
        workspace: ws.dir(),
        pkg_name: &config.pkg_name,
        version: &config.version,
        // The `python-` prefix is set by `Ecosystem::rpm_name` (see
        // `build/src/sources.rs`) once the probe has decided the ecosystem,
        // so it is the single stable "this is a Python package" signal the
        // pipeline can hand the agent prompt.
        is_python: config.rpm_name.starts_with("python-") && !config.unknown_build_system,
        task: ctx.task,
        attempt: ctx.attempt,
        max_attempts: config.max_attempts,
        spent_usd: spent,
        budget_usd: config.budget_usd,
        hint: ctx.hint,
        notes: &notes,
        package_notes: &package_notes,
        from_repo_archive: ctx.from_repo_archive,
        subprojects: &config.subprojects,
        unresolvable_deps: ctx.unresolvable_deps,
        unresolvable_detail: ctx.unresolvable_detail,
        failed_deps: &config.failed_deps,
        unbuildable_deps: &config.unbuildable_deps,
        // A session is only resumed on the model that wrote it: another model
        // has no prompt cache for that history, so it would re-bill all of it
        // at the uncached rate (an Opus turn resuming a Sonnet session cost
        // $1.63 for 2k output tokens).
        session_id: if ctx.resume && !fresh_session.get() {
            session_id.as_deref().and_then(|s| {
                let (model, id) = s.split_once('|')?;
                (model == crate::agent::model_for_task(ctx.task, ctx.attempt, model_rotation.get()))
                    .then_some(id)
            })
        } else {
            None
        },
        checkout_dir: ctx.checkout_dir,
        context: nudge.get().map(String::as_str).or(base_context.as_deref()),
        carried: ctx.carried,
        rpms_dir: Some(&config.rpms_dir),
        sibling_dirs: &sibling_dirs,
        model_rotation: model_rotation.get(),
        writable_patches,
        fetch_hosts: crate::bugfix::fetch_hosts_of(config.bug_fix.as_deref()),
        on_tool_use: Some(&on_tool_use),
    };

    // A 429 is the provider's quota, not this package's fault. Re-issue the
    // turn on the next model in the rotation (each model has its own quota)
    // after the pause every turn shares, before spending one of the package's
    // attempts, so a burst of parallel packages does not all give up at once.
    let mut rate_limit_retries = 0u32;
    let turn_result = loop {
        // The quota is shared by every package running in parallel: when any
        // turn hits a 429, all of them pause instead of each discovering it
        // separately and retrying in lockstep.
        let slot = AgentSlot::acquire();
        let mut result = crate::agent::turn(make_req(*spent_usd));
        charge_failed_call(
            &config.work_dir,
            ws,
            &config.run_id,
            spent_usd,
            &result,
            reporter,
            step.id,
        );
        // A resumed session can answer in prose ("I already called
        // StructuredOutput in my previous message") without making the call in
        // THIS turn, which leaves nothing to act on (python-brotlicffi's last
        // failure). Ask once more in a fresh session, saying what happened.
        if matches!(&result, Err(crate::agent::Error::Parse { message, .. })
            if message.contains("no usable turn response"))
        {
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: "the agent ended its turn without calling StructuredOutput; \
                          retrying once in a fresh session"
                    .to_string(),
            });
            // The retry keeps the same context as the first try, fact blocks
            // (licence guard, sibling checkouts) included.
            let _ = nudge.set(format!(
                "{}{STRUCTURED_OUTPUT_NUDGE}",
                base_context
                    .as_deref()
                    .map(|c| format!("{c}\n\n"))
                    .unwrap_or_default()
            ));
            fresh_session.set(true);
            result = crate::agent::turn(make_req(*spent_usd));
            charge_failed_call(
                &config.work_dir,
                ws,
                &config.run_id,
                spent_usd,
                &result,
                reporter,
                step.id,
            );
        }
        drop(slot);
        if matches!(&result, Err(e) if e.is_rate_limited()) {
            AgentSlot::note_rate_limited();
        } else if result.is_ok() {
            AgentSlot::note_turn_ok();
        }
        match result {
            Err(e)
                if e.is_rate_limited() && rate_limit_retries < MAX_IMMEDIATE_RATE_LIMIT_RETRIES =>
            {
                rate_limit_retries += 1;
                model_rotation.set(model_rotation.get() + 1);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: format!(
                        "model API rate-limited (429) on {}; pausing, then retrying on {} ({rate_limit_retries}/{MAX_IMMEDIATE_RATE_LIMIT_RETRIES})",
                        crate::agent::model_for_task(ctx.task, ctx.attempt, model_rotation.get() - 1),
                        crate::agent::model_for_task(ctx.task, ctx.attempt, model_rotation.get()),
                    ),
                });
            }
            other => break other,
        }
    };
    let outcome = match turn_result {
        Ok(o) => o,
        Err(e) => {
            // Any of these failing is otherwise a dead end: nothing below
            // this point runs, so without this the prompt, stdout and stderr
            // all vanish the moment this process exits and the failure
            // becomes undiagnosable — exactly what happened to the
            // `python-torchcodec`/`python-backports-zstd` "claude exited 1"
            // crashes that prompted adding this (see `agent::Error`'s
            // accessors for which variants carry what).
            for (name, content) in [
                ("crash-prompt.md", e.prompt()),
                ("crash-stdout.log", e.stdout()),
                ("crash-stderr.log", e.stderr()),
            ] {
                let Some(content) = content.filter(|c| !c.is_empty()) else {
                    continue;
                };
                let path = step.file(name);
                if ap_core::fsx::write_atomic(&path, content).is_ok() {
                    reporter.report(&Event::Artifact {
                        step: step.id,
                        name: name.to_string(),
                        path,
                    });
                }
            }
            let step = step.finish(StepOutcome::Failed);
            let message = friendly_agent_error(
                &e,
                config.budget_usd,
                &config.pkg_name,
                crate::agent::max_agent_turns(ctx.task),
            );
            let kind = turn_failure_kind(&e);
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: message.clone(),
            });
            return Err((message, kind));
        }
    };

    // The turn's cost is charged first: a turn stopped by the check below cost it.
    *spent_usd += outcome.usage.cost_usd();
    record_daily_spend(&config.work_dir, outcome.usage.cost_usd());
    record_turn(
        &mut signals.lock().unwrap_or_else(|e| e.into_inner()),
        ctx.task,
        &outcome,
    );
    // A turn that changed the Version is undone and stops the package, whatever
    // its action, a wheel switch included.
    if let Some(why) = version_check(
        version_spec.as_deref(),
        &config.version,
        &format!("{}, after the turn", ctx.label),
    ) {
        if let (Some(path), Some(text)) = (version_spec.as_deref(), before_spec.as_deref()) {
            restore_spec(path, text, None, reporter);
        }
        let step = step.finish(StepOutcome::Failed);
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: why.clone(),
        });
        return Err((why, FailureKind::VersionChanged));
    }

    *session_id = outcome.session_id.as_ref().map(|id| {
        format!(
            "{}|{id}",
            crate::agent::model_for_task(ctx.task, ctx.attempt, model_rotation.get())
        )
    });

    // If the claude CLI's final `result` event carried a structured
    // end-early signal but `pick_turn_response` still rescued a usable
    // `StructuredOutput` from the stream, we return Ok but the turn
    // was NOT clean — surface it as a Warning so a budget-cut-but-
    // rescued turn is distinguishable from a clean one in the event
    // stream (otherwise the per-package budget runs out one turn
    // later with no visible warning to the user). The budget-cap case
    // additionally gets the --budget-usd hint, since that is the one
    // knob the user can actually turn.
    if let Some(signal) = &outcome.ended_early {
        let hint = if signal.subtype.starts_with("error_max_budget") {
            format!(
                " Raise --budget-usd (currently ${:.2}) if the next \
                 turn needs more headroom.",
                config.budget_usd
            )
        } else {
            String::new()
        };
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!(
                "claude ended turn early ({}): {} — a StructuredOutput \
                 was rescued from the stream so the turn is being \
                 applied, but the model was cut off before it could \
                 finish.{hint}",
                signal.subtype, signal.message,
            ),
        });
    }

    // Persist the running total so an outer-loop retry (or a crash
    // mid-pipeline) within the SAME CLI invocation resumes with the
    // correct cumulative spend instead of a reset-to-zero cap. Tagged
    // with the current run_id so a fresh CLI invocation reads a
    // mismatch and starts the package at $0 — the user asked for that
    // restart-reset. See `PkgState::cumulative_spent_run_id`.
    if let Ok(ws) = Workspace::open(&config.state_root, &config.rpm_name) {
        // Only the spend fields: the in-memory total is the truth for them,
        // while everything else in the file may have moved under another writer.
        update_state(&ws, reporter, |st| {
            claim_run(st, &config.run_id);
            st.cumulative_spent_usd = *spent_usd;
        });
    }

    // A command the allowlist refused is a gap in the tool, not the package's:
    // say so in the log, so the next run's reader sees what the turn could not do.
    if !outcome.denied_commands.is_empty() {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!(
                "{}: the sandbox refused {} command(s) in this turn: {}",
                config.rpm_name,
                outcome.denied_commands.len(),
                outcome
                    .denied_commands
                    .iter()
                    .map(|c| format!("`{}`", brief(c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    let prompt_path = step.file("prompt.md");
    write_or_warn(&prompt_path, &outcome.prompt);
    write_or_warn(step.file("output.md"), &outcome.output);

    reporter.report(&Event::AgentPrompt {
        step: step.id,
        task: ctx.event_task,
        prompt: outcome.prompt.clone(),
        artifact: prompt_path,
    });

    let response = response_data(&outcome);
    let response_path = step.file("response.json");
    if let Ok(json) = serde_json::to_string_pretty(&response) {
        write_or_warn(&response_path, json);
    }
    reporter.report(&Event::AgentReply {
        step: step.id,
        response,
        raw: outcome.raw.clone(),
        usage: ap_core::event::Usage {
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cost_usd: outcome.usage.cost_usd(),
        },
        artifact: response_path,
    });

    let step = step.finish(StepOutcome::Ok);

    Ok((step, outcome))
}
