//! The plan report of an update, the sweep over dependency failures, and the
//! retry count they allow.

use super::*;

/// The plan of an update, reported one package at a time as the sweep finds it.
#[derive(Default)]
pub(super) struct PlanReport {
    /// Updates the pipeline can build.
    pub(super) n_update: usize,
    /// Newer versions found where the pipeline cannot build them from.
    n_found_only: usize,
    pub(super) n_uptodate: usize,
    n_skipped: usize,
}

impl PlanReport {
    pub(super) fn add(
        &mut self,
        c: &ap_build::update::Candidate,
        reporter: &(dyn Reporter + Send + Sync),
    ) {
        use ap_build::update::Decision;
        let eco_tag = |e: &ap_build::update::Ecosystem| e.tag();
        let line = match &c.decision {
            Decision::Update {
                ecosystem,
                old_version,
                new_version,
            } => {
                let note = if ecosystem.can_build() {
                    self.n_update += 1;
                    ""
                } else {
                    self.n_found_only += 1;
                    ", not built: the pipeline cannot fetch this source yet"
                };
                format!(
                    "[update]  {}  {} → {}  ({}{note})",
                    c.rpm_name,
                    old_version,
                    new_version,
                    eco_tag(ecosystem),
                )
            }
            Decision::UpToDate { ecosystem, version } => {
                self.n_uptodate += 1;
                format!(
                    "[current] {}  {}  ({})",
                    c.rpm_name,
                    version,
                    eco_tag(ecosystem)
                )
            }
            Decision::Skipped(reason) => {
                self.n_skipped += 1;
                format!("[skip]    {}  ({reason})", c.rpm_name)
            }
        };
        reporter.report(&Event::Progress {
            step: None,
            message: line,
        });
    }

    pub(super) fn total(&self) -> usize {
        self.n_update + self.n_found_only + self.n_uptodate + self.n_skipped
    }

    pub(super) fn finish(&self, reporter: &(dyn Reporter + Send + Sync)) {
        reporter.report(&Event::Progress {
            step: None,
            message: format!(
                "update plan: {} to update, {} newer but not buildable, {} up to date, {} skipped",
                self.n_update, self.n_found_only, self.n_uptodate, self.n_skipped
            ),
        });
    }
}

/// The sweep of a project for newer releases, running on its own thread so
/// that the packages it finds can be branched and built while it goes on with
/// the rest. Each package that has an update arrives on `updates`.
pub(super) struct Sweep {
    pub(super) updates: std::sync::mpsc::Receiver<ap_build::update::Candidate>,
    handle: std::thread::JoinHandle<Result<PlanReport, String>>,
}

impl Sweep {
    pub(super) fn start(
        project: &str,
        only: &[String],
        exclude: &[String],
        reporter: Arc<dyn Reporter + Send + Sync>,
    ) -> Self {
        let (tx, updates) = std::sync::mpsc::channel();
        let (project, only, exclude) = (project.to_string(), only.to_vec(), exclude.to_vec());
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            ap_build::update::plan_each(
                &project,
                &|n| update_project::wanted_by(&only, &exclude, n),
                ap_build::proc::Emit::run(reporter.as_ref()),
                &mut |c| {
                    plan.add(&c, reporter.as_ref());
                    let is_update = matches!(
                        &c.decision,
                        ap_build::update::Decision::Update { ecosystem, .. } if ecosystem.can_build()
                    );
                    // A closed channel means the run gave up: stop checking.
                    !is_update || tx.send(c).is_ok()
                },
            )?;
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self { updates, handle }
    }

    /// The plan for the one package `--only` names, read at `revision` and
    /// aimed at `to_version` when given (`update --revision/--to-version`). It
    /// arrives on `updates` like a sweep's, so the rest of the run is the same.
    pub(super) fn start_pinned(
        project: &str,
        only: &[String],
        revision: Option<String>,
        to_version: Option<String>,
        reporter: Arc<dyn Reporter + Send + Sync>,
    ) -> Self {
        let (tx, updates) = std::sync::mpsc::channel();
        let (project, only) = (project.to_string(), only.to_vec());
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            let emit = ap_build::proc::Emit::run(reporter.as_ref());
            let c = ap_build::branch_revision::plan_one(
                &project,
                &|n| update_project::wanted_by(&only, &[], n),
                revision.as_deref(),
                to_version.as_deref(),
                emit,
            )?;
            plan.add(&c, reporter.as_ref());
            if let ap_build::update::Decision::Update {
                ecosystem,
                old_version,
                new_version,
            } = &c.decision
                && ecosystem.can_build()
            {
                if to_version.is_some() || revision.is_some() {
                    reporter.report(&Event::Progress {
                        step: None,
                        message: ap_build::branch_revision::forced_line(
                            &c.rpm_name,
                            old_version,
                            new_version,
                            revision.as_deref(),
                        ),
                    });
                }
                let _ = tx.send(c);
            }
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self { updates, handle }
    }

    /// How the sweep ended, once `updates` has run dry.
    pub(super) fn finish(self) -> Result<PlanReport, String> {
        drop(self.updates);
        self.handle
            .join()
            .unwrap_or_else(|_| Err("the sweep of the project panicked".to_string()))
    }
}

/// Whether a failed dependency run is queued again within this run. A failure
/// to meet a ship condition is: the package's own condition counters (two
/// patch turns, then the upstream wheel) bound it, so it neither waits for a
/// dependent to ask in a later run nor depends on the round count. Other
/// failures are retried within `max_attempts` rounds, except a spent allowance
/// or a rate limit, which a repeat would only reproduce.
pub(super) fn dep_failure_retries(kind: FailureKind, rounds: usize, max_attempts: usize) -> bool {
    match kind {
        FailureKind::ConditionUnmet => true,
        FailureKind::Exhausted | FailureKind::RateLimited => false,
        _ => rounds < max_attempts,
    }
}
