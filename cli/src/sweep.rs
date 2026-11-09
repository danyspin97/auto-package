//! The plan report of an update, the sweep over dependency failures, and the
//! retry count they allow.

use super::*;

mod hand;
use hand::Hand;

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
        self.report_progress(reporter, false, Some(swept_package(c)));
    }

    fn report_progress(
        &self,
        reporter: &(dyn Reporter + Send + Sync),
        finished: bool,
        package: Option<ap_core::sweep_event::SweptPackage>,
    ) {
        reporter.report(&Event::SweepProgress {
            checked: self.total(),
            to_update: self.n_update + self.n_found_only,
            up_to_date: self.n_uptodate,
            skipped: self.n_skipped,
            finished,
            package,
            total: None,
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
        self.report_progress(reporter, true, None);
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

/// What the check of `c` found, under the name the package is reported by
/// from here on: an update the pipeline builds takes the name of its build's
/// row (see `update_project::row_name`), so its later events land on the row.
fn swept_package(c: &ap_build::update::Candidate) -> ap_core::sweep_event::SweptPackage {
    use ap_build::update::Decision;
    use ap_core::sweep_event::{SweepVerdict, SweptPackage};
    let verdict = match &c.decision {
        Decision::Update {
            old_version,
            new_version,
            ..
        } => SweepVerdict::Update {
            from: old_version.clone(),
            to: new_version.clone(),
        },
        Decision::UpToDate { version, .. } => SweepVerdict::UpToDate {
            version: version.clone(),
        },
        Decision::Skipped(reason) => SweepVerdict::Skipped {
            reason: reason.to_string(),
        },
    };
    let built = ap_build::update_project::items_from_plan("", std::slice::from_ref(c), &[]);
    let name = built
        .first()
        .and_then(update_project::row_name)
        .unwrap_or_else(|| c.rpm_name.clone());
    SweptPackage { name, verdict }
}

/// The sweep of a project for newer releases, running on its own thread so
/// that the packages it finds can be branched and built while it goes on with
/// the rest. Each package that has an update arrives on `updates`.
/// `queue`, when given, is told each one is queued as soon as its check
/// finds it (see `update_project::queue_found`).
pub(super) struct Sweep {
    pub(super) updates: std::sync::mpsc::Receiver<ap_build::update::Candidate>,
    hand: Arc<Hand>,
    handle: std::thread::JoinHandle<Result<PlanReport, String>>,
}

impl Sweep {
    pub(super) fn start(
        project: &str,
        only: &[String],
        exclude: &[String],
        reporter: Arc<dyn Reporter + Send + Sync>,
        queue: Option<Arc<dyn Reporter + Send + Sync>>,
    ) -> Self {
        let (tx, updates) = std::sync::mpsc::channel();
        let hand = Arc::new(Hand::new(tx));
        let (project, only, exclude) = (project.to_string(), only.to_vec(), exclude.to_vec());
        let thread_hand = hand.clone();
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            let swept = ap_build::update::plan_each(
                &project,
                &|n| update_project::wanted_by(&only, &exclude, n),
                ap_build::proc::Emit::run(reporter.as_ref()),
                &mut |c| {
                    plan.add(&c, reporter.as_ref());
                    let is_update = matches!(
                        &c.decision,
                        ap_build::update::Decision::Update { ecosystem, .. } if ecosystem.can_build()
                    );
                    // A closed hand-over means the run gave up: stop checking.
                    !is_update
                        || thread_hand.hand_over(c, |c| {
                            if let Some(queue) = &queue {
                                update_project::queue_found(queue.as_ref(), &project, c, &exclude);
                            }
                        })
                },
            );
            // The channel ends with the sweep, whether it finished or failed.
            thread_hand.close();
            swept?;
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self {
            updates,
            hand,
            handle,
        }
    }

    /// Stops the sweep: nothing more is queued or handed over, and the
    /// candidates found but not taken are returned, in the order found.
    pub(super) fn stop(&self) -> Vec<ap_build::update::Candidate> {
        self.hand.close();
        self.updates.try_iter().collect()
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
        let hand = Arc::new(Hand::new(tx));
        let thread_hand = hand.clone();
        let (project, only) = (project.to_string(), only.to_vec());
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            let emit = ap_build::proc::Emit::run(reporter.as_ref());
            let planned = ap_build::branch_revision::plan_one(
                &project,
                &|n| update_project::wanted_by(&only, &[], n),
                revision.as_deref(),
                to_version.as_deref(),
                emit,
            );
            let c = match planned {
                Ok(c) => c,
                Err(e) => {
                    thread_hand.close();
                    return Err(e);
                }
            };
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
                let _ = thread_hand.hand_over(c, |_| {});
            }
            thread_hand.close();
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self {
            updates,
            hand,
            handle,
        }
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

#[cfg(test)]
mod swept_package_tests {
    use super::swept_package;
    use ap_build::update::{Candidate, Decision, Ecosystem, SkipReason};
    use ap_core::sweep_event::SweepVerdict;

    fn candidate(name: &str, decision: Decision) -> Candidate {
        Candidate {
            rpm_name: name.to_string(),
            decision,
            notes: Vec::new(),
        }
    }

    fn update(ecosystem: Ecosystem) -> Decision {
        Decision::Update {
            ecosystem,
            old_version: "1.0".into(),
            new_version: "2.0".into(),
            upstream_tag: None,
        }
    }

    #[test]
    fn an_update_the_pipeline_builds_is_named_as_its_build_row() {
        let pypi = candidate(
            "python-APScheduler",
            update(Ecosystem::Pypi {
                pypi_name: "APScheduler".into(),
            }),
        );
        let swept = swept_package(&pypi);
        assert_eq!(swept.name, "python-apscheduler");
        assert_eq!(
            swept.verdict,
            SweepVerdict::Update {
                from: "1.0".into(),
                to: "2.0".into()
            }
        );
        // Not buildable: there is no build row, the OBS name stays.
        let repology = candidate(
            "Foo",
            update(Ecosystem::Repology {
                project: "foo".into(),
            }),
        );
        assert_eq!(swept_package(&repology).name, "Foo");
    }

    #[test]
    fn a_current_or_skipped_package_keeps_its_obs_name() {
        let current = candidate(
            "Foo",
            Decision::UpToDate {
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "foo".into(),
                },
                version: "1.0".into(),
            },
        );
        let swept = swept_package(&current);
        assert_eq!(swept.name, "Foo");
        assert_eq!(
            swept.verdict,
            SweepVerdict::UpToDate {
                version: "1.0".into()
            }
        );
        let skipped = swept_package(&candidate("bar", Decision::Skipped(SkipReason::NoSpec)));
        assert_eq!(skipped.name, "bar");
        assert!(matches!(skipped.verdict, SweepVerdict::Skipped { .. }));
    }
}
