//! The events that only concern one package's row: its steps, agent turns,
//! commands and log lines.

use super::*;

impl TuiState {
    pub(super) fn apply_pkg_event(&mut self, pkg: &str, event: &Event) {
        let state = self.pkg_mut(pkg);
        match event {
            Event::StepStarted {
                kind,
                attempt,
                round,
                ..
            } => {
                // The flag belongs to the failure that hit ENOSPC. Only the
                // failure-analysis and agent steps that follow that build
                // may still report it; any other step starts new work whose
                // failure must not be blamed on the disk.
                if !kind.spec().keeps_disk_flag {
                    state.disk_full_detected = false;
                }
                // The notes turn only runs after a successful build, so the package
                // is already done; showing it as running would hide that.
                if state.status == PkgStatus::Queued {
                    state.started = Instant::now();
                }
                if *kind != StepKind::PackageNotes {
                    state.ended = None;
                }
                state.status = if *kind == StepKind::PackageNotes {
                    PkgStatus::Succeeded
                } else {
                    PkgStatus::Running
                };
                state.step = Some(StepInfo {
                    kind: *kind,
                    attempt: *attempt,
                    round: *round,
                    started: Instant::now(),
                    done: None,
                });
                let header = if *attempt > 1 {
                    format!("▸ {} #{attempt}", kind.label())
                } else {
                    format!("▸ {}", kind.label())
                };
                state.add_step_header(header);
            }
            // Never a step of its own: the current step stays on screen.
            Event::StepSkipped { kind, reason } => {
                state.skipped.push((*kind, reason.clone()));
                state.add_log(
                    LogLevel::Warn,
                    format!("⇥ Skipped {}: {reason}", kind.label()),
                );
            }
            Event::StepFinished {
                outcome, elapsed, ..
            } => {
                let (level, marker) = match outcome {
                    StepOutcome::Ok => (LogLevel::Success, "✓"),
                    StepOutcome::Failed => (LogLevel::Error, "✗"),
                    StepOutcome::Skipped => (LogLevel::Warn, "⇥"),
                };
                state.add_log(
                    level,
                    format!("  {marker} {outcome} in {:.2}s", elapsed.as_secs_f64()),
                );
                // Between steps (or before the agent turn / PackageFinished):
                // the step stays on screen with its final time.
                if let Some(step) = state.step.as_mut() {
                    step.done = Some((outcome.clone(), *elapsed));
                    // A failed agent turn is a different failure than the
                    // ENOSPC build that preceded it.
                    if step.kind.spec().keeps_disk_flag && matches!(outcome, StepOutcome::Failed) {
                        state.disk_full_detected = false;
                    }
                }
                // The notes turn is the last thing a successful package does: once it
                // ends, the row is done and must not keep showing the notes step
                // (a missing PackageFinished would otherwise leave that text).
                if state
                    .step
                    .as_ref()
                    .is_some_and(|s| s.kind == StepKind::PackageNotes)
                {
                    state.step = None;
                    state.status = PkgStatus::Succeeded;
                } else {
                    state.status = PkgStatus::Between;
                }
            }
            Event::BuildFailed { classification, .. } => {
                state.add_log(LogLevel::Error, format!("Build failed: {classification:?}"));
            }
            Event::ExtractResult { extract, .. } => {
                if extract.disk_full {
                    state.disk_full_detected = true;
                }
                let phase = extract.phase.as_deref().unwrap_or("unknown phase");
                let mut msg = format!("  ↳ failure located in {phase}");
                if let Some(genre) = &extract.genre {
                    msg.push_str(&format!(" ({genre})"));
                }
                if !extract.has_signal {
                    msg.push_str(" — no clear signal, showing log tail");
                }
                state.add_log(LogLevel::Warn, msg);
            }
            Event::CommandDone { code, elapsed, .. } => {
                let level = if *code == 0 {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                };
                state.add_log(
                    level,
                    format!("  = exit {code} ({:.2}s)", elapsed.as_secs_f64()),
                );
            }
            Event::AgentPrompt { task, .. } => {
                let task_str = match task {
                    Task::Review => "Reviewing spec".to_string(),
                    Task::FixAfterBuild {
                        attempt,
                        max_attempts,
                    } => {
                        state.tries = *attempt;
                        state.max_tries = *max_attempts;
                        format!("Fixing spec (attempt {attempt}/{max_attempts})")
                    }
                    Task::TranslateUnresolvableDeps => "Looking up dependency names".to_string(),
                    Task::PatchDependency => "Patching a dependency".to_string(),
                    Task::PostBuildReview => "Reviewing the built spec".to_string(),
                    Task::PatchReview => "Reviewing a rewritten patch".to_string(),
                    Task::FixBug => "Fixing a reported bug".to_string(),
                    Task::FixFindings => "Fixing a request's findings".to_string(),
                };
                state.add_agent_activity(ActivityKind::Task, task_str.clone());
                state.add_log(LogLevel::Info, format!("Agent: {task_str}"));
            }
            Event::NotesUsage { usage, .. } | Event::FailedTurnUsage { usage, .. } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
            }
            Event::AgentReply {
                response, usage, ..
            } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
                // The reasoning goes in the entry too, after a line break, so
                // Enter can show the whole decision; the list shows only the
                // first line.
                state.add_agent_activity(
                    ActivityKind::Reply,
                    format!(
                        "{} (confidence: {}, ${:.2})\n{}",
                        response.action,
                        response.confidence.as_deref().unwrap_or("?"),
                        usage.cost_usd,
                        response.reasoning
                    ),
                );
                let reasoning = truncate_with_ellipsis(&response.reasoning, 80);
                state.add_log(
                    LogLevel::Info,
                    format!("Agent {}: {reasoning}", response.action),
                );
            }
            Event::AgentToolUse { tool, input, .. } => {
                state.add_agent_activity(ActivityKind::ToolUse, format!("→ {tool} {input}"));
            }
            Event::Progress { message, .. } => {
                state.add_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } => {
                state.add_log(LogLevel::Warn, message.clone());
            }
            Event::MissingPackages { names, .. } => {
                state.add_log(
                    LogLevel::Warn,
                    format!("Missing packages: {}", names.join(", ")),
                );
            }
            Event::SpecChanged {
                lines_added,
                lines_removed,
                diff,
                ..
            } => {
                if !diff.is_empty() {
                    state.spec_diffs.push(diff.clone());
                    if state.spec_diffs.len() > MAX_SPEC_DIFFS {
                        state.spec_diffs.remove(0);
                    }
                }
                if *lines_added > 0 || *lines_removed > 0 {
                    state.add_log(
                        LogLevel::Success,
                        format!("Spec changed: +{lines_added} -{lines_removed}"),
                    );
                }
            }
            Event::SpecRejected { reason, .. } => {
                state.add_log(LogLevel::Warn, format!("Spec change rejected: {reason:?}"));
            }
            Event::Command { program, argv, .. } => {
                let cmd = format!("{program} {}", argv.join(" "));
                state.add_log(
                    LogLevel::Info,
                    format!("$ {}", truncate_with_ellipsis(&cmd, 100)),
                );
            }
            Event::Artifact { name, .. } => {
                state.add_log(LogLevel::Success, format!("Artifact: {name}"));
            }
            _ => {}
        }
    }
}
