use super::*;
use ap_core::event::ArchCheckResult;
use ap_core::sweep_event::{SweepVerdict, SweptPackage};

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
    /// The update sweep so far (`Event::SweepProgress`): what was compared and
    /// found, shown in the header since only the packages to update get a row.
    pub(super) sweep: Option<SweepCounts>,
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
            sweep: None,
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
    /// off, everything except ones that built fine or were checked up to
    /// date, which need no further attention. Failed and missing-deps
    /// packages stay visible since they may still need attention or get
    /// retried; the header's `n/N packages done` count still includes the
    /// hidden ones (it is derived from `packages`, not this list). Hiding
    /// them is only the default — `show_all` (toggled with `a`) lets you
    /// spot-check a specific package that already succeeded instead of
    /// having to trust the aggregate count with no way to see which ones.
    ///
    /// The rows are sorted by their status (see [`PkgStatus::sort_rank`]):
    /// running first, then queued, then the ones that ended. Rows of one
    /// rank keep the order they first appeared in, so a long queue does not
    /// reshuffle; the selection follows its package by name (see
    /// `sync_selection`).
    pub(super) fn visible_order(&self) -> Vec<&String> {
        let mut rows: Vec<&String> = self
            .order
            .iter()
            .filter(|name| {
                let status = &self.packages[*name].status;
                self.show_all || !(status.is_built() || *status == PkgStatus::UpToDate)
            })
            .collect();
        rows.sort_by_key(|name| self.packages[*name].status.sort_rank());
        rows
    }

    /// The rows a build was queued for: every row but the ones that are only
    /// what the sweep's check found, and the ones the preparation dropped.
    pub(super) fn build_rows(&self) -> impl Iterator<Item = &PkgState> {
        self.packages
            .values()
            .filter(|p| !p.status.is_check_only() && !p.dropped)
    }

    /// The rows in each stage, counted from their statuses on every call so
    /// the header cannot drift from the table.
    pub(super) fn stage_counts(&self) -> StageCounts {
        StageCounts::of(self.packages.values().map(|p| &p.status))
    }

    /// The row of a package whose check has begun, created `checking` when it
    /// has none. A build's row is not touched.
    fn apply_checking(&mut self, obs_name: &str) {
        if !self.packages.contains_key(obs_name) {
            self.order.push(obs_name.to_string());
            self.packages
                .insert(obs_name.to_string(), PkgState::checking());
            return;
        }
        if let Some(row) = self.packages.get_mut(obs_name) {
            row.advance(Input::Checking);
        }
    }

    /// Removes the checks begun and never answered (the `checking` rows): once
    /// the sweep is finished no answer comes for them, and each would read
    /// `checking` for the rest of the run.
    fn drop_unanswered_checks(&mut self) {
        let unanswered: Vec<String> = self
            .packages
            .iter()
            .filter(|(_, row)| row.status == PkgStatus::Checking)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &unanswered {
            self.packages.remove(name);
        }
        self.order.retain(|name| !unanswered.contains(name));
    }

    /// The row of a package the sweep checked, created when it has none.
    /// What the check found shows only on a row no build has taken over.
    fn apply_check(&mut self, swept: &SweptPackage) {
        // A row begun under the OBS name moves to the name its answer gives,
        // so the row that said `checking` is not left behind.
        if !swept.obs_name.is_empty()
            && swept.obs_name != swept.name
            && self
                .packages
                .get(&swept.obs_name)
                .is_some_and(|row| row.status == PkgStatus::Checking)
        {
            self.packages.remove(&swept.obs_name);
            self.order.retain(|name| *name != swept.obs_name);
        }
        if !self.packages.contains_key(&swept.name) {
            self.order.push(swept.name.clone());
            self.packages
                .insert(swept.name.clone(), PkgState::checking());
        }
        let Some(row) = self.packages.get_mut(&swept.name) else {
            return;
        };
        row.advance(Input::Checked(&swept.verdict));
        if !row.status.is_check_only() {
            return;
        }
        match &swept.verdict {
            SweepVerdict::UpToDate { version } => {
                row.version = version.clone();
                row.from_version = None;
            }
            SweepVerdict::Update { from, to } => {
                row.version = to.clone();
                row.from_version = Some(from.clone());
            }
            SweepVerdict::Skipped { reason } => row.skip_reason = Some(reason.clone()),
        }
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
            Event::SweepProgress {
                checked,
                total,
                to_update,
                up_to_date,
                finished,
                package,
                ..
            } => {
                if let Some(package) = package {
                    self.apply_check(package);
                }
                if *finished {
                    self.drop_unanswered_checks();
                }
                self.sweep = Some(SweepCounts {
                    checked: *checked,
                    total: *total,
                    to_update: *to_update,
                    up_to_date: *up_to_date,
                    finished: *finished,
                });
            }
            Event::SweepChecking { package } => self.apply_checking(package),
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
                state.advance(Input::Selected);
                state.dropped = false;
                state.step = None;
                state.fail_reason = None;
                state.skip_reason = None;
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
            }
            | Event::PackageDropped {
                pkg,
                version,
                outcome,
            } => {
                let dropped = matches!(msg.event, Event::PackageDropped { .. });
                let state = self.pkg_mut(pkg);
                if state.version.is_empty() {
                    state.version = version.clone();
                }
                state.finished = true;
                state.dropped = dropped;
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
                let disk_full = state.disk_full_detected;
                state.advance(Input::Finished { outcome, disk_full });
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
                    PackageOutcome::Skipped { reason } => {
                        state.step = None;
                        state.fail_reason = None;
                        state.skip_reason = Some(reason.clone());
                    }
                    PackageOutcome::Built
                    | PackageOutcome::BuiltLocally
                    | PackageOutcome::AlreadyBuilt
                    | PackageOutcome::MissingDeps { .. } => {
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
