//! Scoped reporting of one pipeline step.
//!
//! A [`StepGuard`] owns the step directory and the `StepStarted` /
//! `StepFinished` pair, so an early `return` or `?` between the two can no
//! longer leave a started step unfinished.

use std::collections::{HashMap, HashSet};
use std::io;
use std::ops::Deref;
use std::time::Instant;

use ap_core::event::{Event, SkipReason, StepKind, StepOutcome};
use ap_core::run::{PkgRunDir, StepDir};
use ap_core::sink::Reporter;

/// How often each kind has started in this package run, and which iteration
/// of the validate/build loop is current.
#[derive(Debug, Default)]
pub struct Attempts {
    counts: HashMap<StepKind, u32>,
    /// Kinds whose skip was already reported since their last start.
    skipped: HashSet<StepKind>,
    pub round: u32,
}

impl Attempts {
    /// Count a start of `kind` and return its 1-based attempt number.
    fn next(&mut self, kind: StepKind) -> u32 {
        // A new attempt may be skipped again; only repeats within one attempt
        // are duplicates.
        self.skipped.remove(&kind);
        let n = self.counts.entry(kind).or_insert(0);
        *n += 1;
        *n
    }

    /// Report that a conditional step did not run, so the event stream and the
    /// TUI show why rather than just a gap. At most one skip is reported per
    /// attempt: several code paths can each decide the same step is skipped,
    /// and the first reason is the one that applied.
    pub fn skip(&mut self, reporter: &dyn Reporter, kind: StepKind, reason: SkipReason) {
        if self.skipped.insert(kind) {
            reporter.report(&Event::StepSkipped { kind, reason });
        }
    }
}

pub struct StepGuard<'a> {
    dir: StepDir,
    reporter: &'a dyn Reporter,
    started: Instant,
    finished: bool,
}

impl<'a> StepGuard<'a> {
    /// Allocate the step directory (`NN-<dir>[-<detail>]`) and report the start.
    pub fn begin(
        pkg_run: &PkgRunDir,
        reporter: &'a dyn Reporter,
        kind: StepKind,
        detail: Option<&str>,
        attempts: &mut Attempts,
    ) -> io::Result<Self> {
        let spec = kind.spec();
        let dir_name = match detail {
            Some(d) => format!("{}-{d}", spec.dir),
            None => spec.dir.to_string(),
        };
        let dir = pkg_run.step(&dir_name)?;
        reporter.report(&Event::StepStarted {
            step: dir.id,
            kind,
            attempt: attempts.next(kind),
            round: attempts.round,
            detail: detail.map(str::to_string),
        });
        Ok(Self {
            dir,
            reporter,
            started: Instant::now(),
            finished: false,
        })
    }

    /// Report the step's end and hand back its directory.
    pub fn finish(mut self, outcome: StepOutcome) -> StepDir {
        self.report_finished(outcome);
        // `Drop` still runs; `finished` keeps it from reporting again.
        StepDir {
            path: self.dir.path.clone(),
            id: self.dir.id,
        }
    }

    fn report_finished(&mut self, outcome: StepOutcome) {
        self.finished = true;
        self.reporter.report(&Event::StepFinished {
            step: self.dir.id,
            outcome,
            elapsed: self.started.elapsed(),
        });
    }
}

impl Deref for StepGuard<'_> {
    type Target = StepDir;

    fn deref(&self) -> &StepDir {
        &self.dir
    }
}

impl Drop for StepGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.report_finished(StepOutcome::Failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_core::run::RunDir;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Event>>);

    impl Reporter for Recorder {
        fn report(&self, event: &Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn dropping_an_unfinished_guard_reports_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_run = RunDir::allocate(tmp.path()).unwrap().pkg("foo");
        let recorder = Recorder::default();
        let mut attempts = Attempts::default();

        drop(StepGuard::begin(&pkg_run, &recorder, StepKind::Build, None, &mut attempts).unwrap());

        let events = recorder.0.lock().unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                Event::StepStarted { .. },
                Event::StepFinished {
                    outcome: StepOutcome::Failed,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn a_finished_guard_reports_once_and_attempts_count_per_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_run = RunDir::allocate(tmp.path()).unwrap().pkg("foo");
        let recorder = Recorder::default();
        let mut attempts = Attempts::default();

        for _ in 0..2 {
            let step = StepGuard::begin(&pkg_run, &recorder, StepKind::Build, None, &mut attempts)
                .unwrap();
            step.finish(StepOutcome::Ok);
        }
        let agent = StepGuard::begin(
            &pkg_run,
            &recorder,
            StepKind::AgentTurn,
            Some("review"),
            &mut attempts,
        )
        .unwrap();
        assert!(agent.path.ends_with("03-agent-review"));
        agent.finish(StepOutcome::Ok);

        let events = recorder.0.lock().unwrap();
        let started: Vec<(StepKind, u32)> = events
            .iter()
            .filter_map(|e| match e {
                Event::StepStarted { kind, attempt, .. } => Some((*kind, *attempt)),
                _ => None,
            })
            .collect();
        assert_eq!(
            started,
            [
                (StepKind::Build, 1),
                (StepKind::Build, 2),
                (StepKind::AgentTurn, 1)
            ]
        );
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn a_step_is_skipped_at_most_once_per_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_run = RunDir::allocate(tmp.path()).unwrap().pkg("foo");
        let recorder = Recorder::default();
        let mut attempts = Attempts::default();

        attempts.skip(&recorder, StepKind::OscService, SkipReason::ServiceUpToDate);
        attempts.skip(&recorder, StepKind::OscService, SkipReason::NotRust);
        attempts.skip(&recorder, StepKind::AgentTurn, SkipReason::NoAgent);
        // Starting the step opens a new attempt, which may be skipped again.
        StepGuard::begin(
            &pkg_run,
            &recorder,
            StepKind::OscService,
            None,
            &mut attempts,
        )
        .unwrap()
        .finish(StepOutcome::Ok);
        attempts.skip(&recorder, StepKind::OscService, SkipReason::NotRust);

        let events = recorder.0.lock().unwrap();
        let skipped: Vec<(StepKind, String)> = events
            .iter()
            .filter_map(|e| match e {
                Event::StepSkipped { kind, reason } => Some((*kind, reason.to_string())),
                _ => None,
            })
            .collect();
        assert_eq!(skipped.len(), 3);
        assert_eq!(skipped[0].0, StepKind::OscService);
        assert_eq!(skipped[0].1, SkipReason::ServiceUpToDate.to_string());
        assert_eq!(skipped[1].0, StepKind::AgentTurn);
        assert_eq!(skipped[2].1, SkipReason::NotRust.to_string());
    }
}
