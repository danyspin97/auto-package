use super::*;
use ap_core::event::ArchCheckResult;

/// Compact duration for the Step column: `42s`, `1m23s`.
pub(super) fn fmt_dur(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

pub(super) struct TuiState {
    /// Project and requested packages, shown in the summary header.
    pub(super) scope: RunScope,
    pub(super) order: Vec<String>,
    pub(super) packages: HashMap<String, PkgState>,
    // Only the most recent run-level message is ever shown (the footer),
    // so there is nothing to gain from keeping a bounded history here the
    // way per-package logs do.
    pub(super) last_global_log: Option<LogLine>,
    /// Run-level messages, newest last: what the Event Log shows while no
    /// package is selected, such as the sweep that plans an `update` before
    /// any package has a row.
    pub(super) run_log: VecDeque<LogLine>,
    pub(super) run_outcome: Option<RunOutcome>,
    /// Whether the run takes the `n` and `r` keys (see `TuiController::new`).
    pub(super) accepts_requests: bool,
    /// Whether a backend can read diffs and make submit requests (the `S` and
    /// `A` keys).
    pub(super) can_submit: bool,
    pub(super) run_start: Option<Instant>,
    /// Toggled with `a` — on by default, so a package that succeeded stays in
    /// the list; off hides the ones `visible_order` would drop once they succeed.
    pub(super) show_all: bool,
    /// Full pip-closure size from the most recent `ResolveFinished` — there
    /// is no live equivalent (see that event's handler), so this is kept as
    /// a point-in-time value rather than derived every frame like `built`.
    pub(super) resolve_total: Option<usize>,
    /// Same snapshot caveat as `resolve_total`: how many of the closure the
    /// distro already satisfies, as of the last resolve pass.
    pub(super) resolve_in_distro: Option<usize>,
}

impl TuiState {
    pub(super) fn new(scope: RunScope) -> Self {
        Self {
            scope,
            order: Vec::new(),
            packages: HashMap::new(),
            last_global_log: None,
            run_log: VecDeque::new(),
            run_outcome: None,
            accepts_requests: false,
            can_submit: false,
            resolve_total: None,
            resolve_in_distro: None,
            run_start: None,
            show_all: true,
        }
    }

    /// Total cost across every package tracked so far. Derived from the
    /// per-package totals (updated live on every `AgentReply`) rather than
    /// cached separately, so the header reflects spend in real time instead
    /// of only jumping to the right number once `RunFinished` arrives.
    pub(super) fn total_cost_usd(&self) -> f64 {
        self.packages.values().map(|p| p.cost_usd).sum()
    }

    pub(super) fn add_global_log(&mut self, level: LogLevel, message: String) {
        let line = LogLine {
            level,
            message,
            emphasis: false,
        };
        self.run_log.push_back(line.clone());
        if self.run_log.len() > MAX_LOG_LINES {
            self.run_log.pop_front();
        }
        self.last_global_log = Some(line);
    }

    /// Packages shown in the table: every one by default, or, with `show_all`
    /// off, everything except ones that built fine and need no further attention. Failed and missing-deps
    /// packages stay visible since they may still need attention or get
    /// retried; the header's `n/N packages done` count still includes the
    /// hidden ones (it is derived from `packages`, not this list). Hiding
    /// them is only the default — `show_all` (toggled with `a`) lets you
    /// spot-check a specific package that already succeeded instead of
    /// having to trust the aggregate count with no way to see which ones.
    pub(super) fn visible_order(&self) -> Vec<&String> {
        self.order
            .iter()
            .filter(|name| {
                self.show_all
                    || !matches!(
                        self.packages[*name].status,
                        PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt
                    )
            })
            .collect()
    }

    /// Packages still to build: rows that are neither finished nor parked on
    /// missing deps after a failure. Derived on every call, like `done`, so it
    /// cannot drift from the rows the way a stored counter could.
    pub(super) fn pending_count(&self) -> usize {
        self.packages
            .values()
            .filter(|p| !p.finished && p.status != PkgStatus::MissingDeps)
            .count()
    }

    pub(super) fn pkg_mut(&mut self, name: &str) -> &mut PkgState {
        self.packages.entry(name.to_string()).or_insert_with(|| {
            self.order.push(name.to_string());
            PkgState::new(String::new())
        })
    }

    pub(super) fn apply(&mut self, msg: Msg) {
        // Events that name their own package (TargetSelected, PackageFinished)
        // are routed by that name regardless of which tap carried them, since
        // they are reported once from the main thread rather than from
        // inside a per-package worker.
        match &msg.event {
            Event::RunStarted { .. } => {
                self.run_start = Some(Instant::now());
                self.add_global_log(LogLevel::Info, "Run started".to_string());
            }
            Event::RunFinished {
                outcome,
                elapsed,
                usage,
            } => {
                self.run_outcome = Some(outcome.clone());
                let level = match outcome {
                    RunOutcome::Success => LogLevel::Success,
                    _ => LogLevel::Error,
                };
                self.add_global_log(
                    level,
                    format!(
                        "Run finished: {outcome} in {:.1}s (${:.2})",
                        elapsed.as_secs_f64(),
                        usage.cost_usd
                    ),
                );
            }
            Event::ResolveStarted { root } => {
                self.add_global_log(LogLevel::Info, format!("Resolving {root}"));
            }
            Event::ResolveFinished {
                total,
                built,
                in_distro,
                remaining,
            } => {
                // `built`/`remaining` here are a snapshot for this one
                // resolve pass only — re-resolving the pip closure is rare
                // (only the current batch's root re-runs `select_target`;
                // the far more frequent ad hoc dependency builds spawned
                // from `UnresolvableDeps`/`MissingDeps` never do), so the
                // header's own `built` figure is computed live from
                // `self.packages` instead (see `render_summary`) rather
                // than trusted from this message — otherwise it reads as
                // stuck at whatever this first snapshot said for most of a
                // long run, long after packages have visibly succeeded in
                // the table. `total`/`in_distro` have no live equivalent
                // (a distro-satisfied dependency is never selected as a
                // target, so it never gets a row here at all) and are kept.
                self.resolve_total = Some(*total);
                self.resolve_in_distro = Some(*in_distro);
                self.add_global_log(
                    LogLevel::Info,
                    format!(
                        "Resolved: {total} total, {built} built, {in_distro} in the distro; {remaining} were still to build when resolved"
                    ),
                );
            }
            Event::TargetSelected {
                pkg,
                version,
                from_version,
                ..
            } => {
                let state = self.pkg_mut(pkg);
                state.version = version.clone();
                state.from_version = from_version.clone();
                // A new attempt: the arches the last build failed on are not
                // known until its check runs again.
                state.arch_fails.clear();
                state.status = PkgStatus::Queued;
                state.started = Instant::now();
                state.step = None;
                state.fail_reason = None;
                state.skipped.clear();
                // A fresh attempt — the condition that caused last time's
                // `NoSpace` may already be gone (that's the whole point of
                // the `r` retry keybinding), so don't carry the flag over
                // and have it immediately reclassify a *new* failure.
                state.disk_full_detected = false;
                // The retry (if that's what this is) has been picked up, and
                // the package is running again: it must not count as done in
                // the header or show the previous attempt's tries.
                state.retry_requested = false;
                state.finished = false;
                state.tries = 0;
                state.max_tries = 0;
                // The package is running again, so it no longer waits on
                // what the previous attempt reported.
                state.waiting_on.clear();
            }
            Event::PackageFinished {
                pkg,
                version,
                outcome,
            } => {
                let state = self.pkg_mut(pkg);
                if state.version.is_empty() {
                    state.version = version.clone();
                }
                state.finished = true;
                state.retry_requested = false;
                state.waiting_on = match outcome {
                    PackageOutcome::MissingDeps { deps } => {
                        let mut deps = deps.clone();
                        deps.sort();
                        deps.dedup();
                        deps
                    }
                    _ => Vec::new(),
                };
                state.status = match outcome {
                    PackageOutcome::Built => PkgStatus::Succeeded,
                    PackageOutcome::BuiltLocally => PkgStatus::BuiltLocally,
                    PackageOutcome::AlreadyBuilt => PkgStatus::AlreadyBuilt,
                    PackageOutcome::MissingDeps { .. } => PkgStatus::MissingDeps,
                    PackageOutcome::Failed { disk_full, .. } => {
                        if state.disk_full_detected || *disk_full {
                            PkgStatus::NoSpace
                        } else {
                            PkgStatus::Failed
                        }
                    }
                    PackageOutcome::NeedsMaintainer { .. } => PkgStatus::Failed,
                    PackageOutcome::Skipped { .. } => PkgStatus::Skipped,
                };
                match outcome {
                    PackageOutcome::Failed { reason, .. }
                    | PackageOutcome::NeedsMaintainer { reason } => {
                        // Keep the failed step so the Step column can name it.
                        // A worker that died mid-step never sent `StepFinished`,
                        // so close the step here.
                        if let Some(step) = state.step.as_mut().filter(|s| s.done.is_none()) {
                            let elapsed = step.started.elapsed();
                            step.done = Some((StepOutcome::Failed, elapsed));
                        }
                        state.fail_reason = Some(reason.clone());
                    }
                    PackageOutcome::Built
                    | PackageOutcome::BuiltLocally
                    | PackageOutcome::AlreadyBuilt
                    | PackageOutcome::MissingDeps { .. }
                    | PackageOutcome::Skipped { .. } => {
                        state.step = None;
                        state.fail_reason = None;
                    }
                }
                let msg = match outcome {
                    PackageOutcome::Built => "built".to_string(),
                    PackageOutcome::BuiltLocally => "built locally".to_string(),
                    PackageOutcome::AlreadyBuilt => "already built".to_string(),
                    PackageOutcome::MissingDeps { deps } => {
                        format!("needs: {}", deps.join(", "))
                    }
                    PackageOutcome::Failed { reason, .. } => format!("failed: {reason}"),
                    PackageOutcome::NeedsMaintainer { reason } => {
                        format!("needs a maintainer: {reason}")
                    }
                    PackageOutcome::Skipped { reason } => format!("skipped: {reason}"),
                };
                state.add_log(outcome_level(outcome), msg);
            }
            // The other-architecture check names its package in the event, and
            // the package's row shows the failing arches as a badge.
            Event::ArchCheck { package, results } => {
                let fails = arch_failures(results);
                let state = self.pkg_mut(package);
                if let Some(badge) = ap_build::arch_check::badge_text(&fails) {
                    state.add_log(LogLevel::Warn, badge);
                }
                state.arch_fails = fails;
            }
            // Run-level progress, from before any package has a row.
            Event::Progress { message, .. } if msg.pkg.is_none() => {
                self.add_global_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } if msg.pkg.is_none() => {
                self.add_global_log(LogLevel::Warn, message.clone());
            }
            // Everything below only makes sense scoped to a package; drop it
            // if it somehow arrives untagged.
            _ => {
                let Some(pkg) = &msg.pkg else { return };
                self.apply_pkg_event(pkg, &msg.event);
            }
        }
    }

    fn apply_pkg_event(&mut self, pkg: &str, event: &Event) {
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

/// The `repo/arch` names of the results an arch check found failing, once each.
fn arch_failures(results: &[ArchCheckResult]) -> Vec<String> {
    let mut fails: Vec<String> = Vec::new();
    for r in results {
        let name = format!("{}/{}", r.repo, r.arch);
        if ap_build::arch_check::is_failure_label(&r.state) && !fails.contains(&name) {
            fails.push(name);
        }
    }
    fails
}

/// The level a package's final outcome is logged at.
fn outcome_level(outcome: &PackageOutcome) -> LogLevel {
    match outcome {
        PackageOutcome::Failed { .. } => LogLevel::Error,
        PackageOutcome::MissingDeps { .. } | PackageOutcome::NeedsMaintainer { .. } => {
            LogLevel::Warn
        }
        PackageOutcome::Skipped { .. } => LogLevel::Info,
        _ => LogLevel::Success,
    }
}

#[cfg(test)]
mod outcome_level_tests {
    use super::*;

    #[test]
    fn a_skip_is_logged_as_information_not_as_success() {
        let skipped = PackageOutcome::Skipped {
            reason: "excluded".into(),
        };
        assert!(matches!(outcome_level(&skipped), LogLevel::Info));
        let built = PackageOutcome::Built;
        assert!(matches!(outcome_level(&built), LogLevel::Success));
    }
}
