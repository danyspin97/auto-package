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
    /// The skipped packages OBS answered 404 for: the subset of `n_skipped`.
    n_not_on_obs: usize,
    /// The packages stopped for the maintainer (a `_link` whose target is
    /// gone), each with its reason.
    pub(super) maintainer: Vec<String>,
    /// The rpm name of every package the sweep found, for the `--only` names
    /// that matched none.
    pub(super) names: Vec<String>,
    /// The packages whose spec OBS failed to answer, each with the reason: they
    /// were not compared, so the run is incomplete.
    pub(super) unreadable: Vec<String>,
    /// The skipped packages counted by their reason (see [`skip_label`]).
    by_reason: std::collections::BTreeMap<String, usize>,
}

/// The label a skip counts under in the plan line: the reason's kind, and
/// `stopped` for a read that the run's interruption ended.
fn skip_label(reason: &ap_build::update::SkipReason) -> String {
    use ap_build::update::SkipReason;
    match reason {
        SkipReason::ReadFailed(why) if why == "interrupted" => "stopped".to_string(),
        other => other.kind().to_string(),
    }
}

impl PlanReport {
    pub(super) fn add(
        &mut self,
        c: &ap_build::update::Candidate,
        reporter: &(dyn Reporter + Send + Sync),
    ) {
        use ap_build::update::{Decision, SkipReason};
        self.names.push(c.rpm_name.clone());
        match &c.decision {
            Decision::Skipped(SkipReason::NeedsMaintainer(why)) => {
                self.maintainer.push(format!("{}: {why}", c.rpm_name));
            }
            Decision::Skipped(SkipReason::ReadFailed(why)) => {
                self.unreadable.push(format!("{}: {why}", c.rpm_name));
            }
            // A lookup that a rate limit or an outage failed may pass: the package
            // is not checked, so the run is incomplete.
            Decision::Skipped(reason) if reason.is_transient_lookup() => {
                self.unreadable.push(format!("{}: {reason}", c.rpm_name));
            }
            Decision::Skipped(SkipReason::NotOnObs(_)) => self.n_not_on_obs += 1,
            Decision::Skipped(reason @ SkipReason::RepologyDisagrees { .. }) => {
                self.maintainer.push(format!("{}: {reason}", c.rpm_name));
            }
            _ => {}
        }
        let eco_tag = |e: &ap_build::update::Ecosystem| e.tag();
        let line = match &c.decision {
            Decision::Update {
                ecosystem,
                old_version,
                new_version,
                ..
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
                *self.by_reason.entry(skip_label(reason)).or_default() += 1;
                format!("[skip]    {}  ({reason})", c.rpm_name)
            }
        };
        reporter.report(&Event::Progress {
            step: None,
            message: line,
        });
        for note in &c.notes {
            reporter.report(&Event::Progress {
                step: None,
                message: format!("[note]    {}  ({note})", c.rpm_name),
            });
        }
        self.report_progress(reporter, false);
    }

    fn report_progress(&self, reporter: &(dyn Reporter + Send + Sync), finished: bool) {
        reporter.report(&Event::SweepProgress {
            checked: self.total(),
            to_update: self.n_update + self.n_found_only,
            up_to_date: self.n_uptodate,
            skipped: self.n_skipped,
            finished,
        });
    }

    pub(super) fn total(&self) -> usize {
        self.n_update + self.n_found_only + self.n_uptodate + self.n_skipped
    }

    /// Whether every package the sweep found is absent from OBS, or it found
    /// none: nothing a run could check was there. A package skipped on purpose
    /// (no spec, a maintainer's call) is a package found.
    pub(super) fn only_absent(&self) -> bool {
        self.total() == self.n_not_on_obs
    }

    pub(super) fn finish(&self, reporter: &(dyn Reporter + Send + Sync)) {
        self.report_progress(reporter, true);
        let stopped = self.by_reason.contains_key("stopped") || ap_build::proc::shutting_down();
        let head = if stopped {
            "update plan, stopped before the end"
        } else {
            "update plan"
        };
        let breakdown = self
            .by_reason
            .iter()
            .map(|(label, n)| format!("{label} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let breakdown = if breakdown.is_empty() {
            String::new()
        } else {
            format!(" ({breakdown})")
        };
        reporter.report(&Event::Progress {
            step: None,
            message: format!(
                "{head}: {} to update, {} newer but not buildable, {} up to date, {} skipped{breakdown}",
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
                ..
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
/// or a rate limit, which a repeat would only reproduce, a build OBS does not
/// make for this arch (excluded or disabled), and a turn rejected
/// for touching the checkout's metadata or the package's Version, which the
/// pipeline does not retry.
pub(super) fn dep_failure_retries(kind: FailureKind, rounds: usize, max_attempts: usize) -> bool {
    match kind {
        FailureKind::ConditionUnmet => true,
        FailureKind::Exhausted
        | FailureKind::RateLimited
        | FailureKind::Excluded
        | FailureKind::Tampered
        | FailureKind::VersionChanged
        | FailureKind::NoFix
        | FailureKind::FixDoesNotApply => false,
        _ => rounds < max_attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::PlanReport;
    use ap_build::update::{Candidate, Decision, SkipReason};
    use ap_core::sink::NullReporter;

    fn skipped(name: &str, reason: SkipReason) -> Candidate {
        Candidate {
            rpm_name: name.to_string(),
            decision: Decision::Skipped(reason),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_failed_read_is_unreadable_and_a_404_is_not_on_obs() {
        let mut plan = PlanReport::default();
        plan.add(
            &skipped("a", SkipReason::ReadFailed("listing failed".into())),
            &NullReporter,
        );
        plan.add(
            &skipped("b", SkipReason::NotOnObs("gone".into())),
            &NullReporter,
        );
        plan.add(&skipped("c", SkipReason::NoSpec), &NullReporter);
        assert_eq!(plan.unreadable, ["a: listing failed"]);
        assert_eq!(plan.n_not_on_obs, 1);
        assert_eq!(plan.total(), 3);
        // NoSpec is a package found on purpose, so the run is not all-absent.
        assert!(!plan.only_absent());
    }

    #[test]
    fn a_sweep_that_found_only_absent_or_no_packages_is_not_a_checked_one() {
        assert!(PlanReport::default().only_absent());
        let absent = PlanReport {
            n_skipped: 2,
            n_not_on_obs: 2,
            ..Default::default()
        };
        assert!(absent.only_absent());
        let mixed = PlanReport {
            n_skipped: 2,
            n_not_on_obs: 1,
            ..Default::default()
        };
        assert!(!mixed.only_absent());
        let skipped_on_purpose = PlanReport {
            n_skipped: 1,
            ..Default::default()
        };
        assert!(!skipped_on_purpose.only_absent());
        let found = PlanReport {
            n_update: 1,
            ..Default::default()
        };
        assert!(!found.only_absent());
    }

    #[test]
    fn the_plan_line_breaks_the_skips_down_by_reason() {
        let mut plan = PlanReport::default();
        plan.add(&skipped("a", SkipReason::NoSpec), &NullReporter);
        plan.add(&skipped("b", SkipReason::NoSpec), &NullReporter);
        plan.add(
            &skipped("c", SkipReason::ReadFailed("interrupted".into())),
            &NullReporter,
        );
        assert_eq!(plan.by_reason.get("NoSpec"), Some(&2));
        assert_eq!(plan.by_reason.get("stopped"), Some(&1));
        assert_eq!(super::skip_label(&SkipReason::NoSpec), "NoSpec");
        let lookup = SkipReason::LookupFailed {
            ecosystem_tag: "PyPI".into(),
            name: "x".into(),
            error: "404".into(),
        };
        assert_eq!(super::skip_label(&lookup), "LookupFailed");
        assert_eq!(SkipReason::NotOnObs("gone".into()).kind(), "NotOnObs");
    }

    #[test]
    fn a_lookup_an_outage_failed_is_incomplete_not_a_skip() {
        let mut plan = PlanReport::default();
        plan.add(
            &skipped(
                "mailchariot",
                SkipReason::LookupFailed {
                    ecosystem_tag: "GitRepo".into(),
                    name: "https://example.org/x".into(),
                    error: "http status: 503".into(),
                },
            ),
            &NullReporter,
        );
        plan.add(
            &skipped(
                "gone",
                SkipReason::LookupFailed {
                    ecosystem_tag: "PyPI".into(),
                    name: "gone".into(),
                    error: "http status: 404".into(),
                },
            ),
            &NullReporter,
        );
        assert_eq!(plan.unreadable.len(), 1);
        assert!(plan.unreadable[0].starts_with("mailchariot: "));
        assert_eq!(plan.n_skipped, 2);
    }
}
