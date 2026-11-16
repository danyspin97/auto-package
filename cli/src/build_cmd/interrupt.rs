//! What an interrupted `build` says it left unfinished: one counted line, as
//! `update` says it (`update_project::report::interrupt_summary`).

use crate::*;
use std::collections::HashSet;

/// The line that says what an interrupt left unfinished, or `None` when
/// nothing was left: the packages selected but never started, and those
/// stopped in flight (their `PackageFinished` says `Interrupted`).
pub(crate) fn interrupt_summary(selected: &[String], finished: &[Event]) -> Option<String> {
    let mut done: HashSet<&str> = HashSet::new();
    let mut stopped: HashSet<&str> = HashSet::new();
    for event in finished {
        if let Event::PackageFinished { pkg, outcome, .. } = event {
            done.insert(pkg.as_str());
            if matches!(outcome, PackageOutcome::Interrupted) {
                stopped.insert(pkg.as_str());
            }
        }
    }
    let not_started = selected
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|pkg| !done.contains(pkg))
        .count();
    let in_flight = stopped.len();
    if not_started + in_flight == 0 {
        return None;
    }
    Some(format!(
        "interrupted: {not_started} package(s) not started, {in_flight} in flight stopped; \
         re-run the same command to continue"
    ))
}

/// Reports the [`interrupt_summary`] of a run as a progress line, so it reaches
/// the run's log and the terminal before `RunFinished`. Nothing is reported
/// unless the run was `stopped` by the user, or when nothing was left.
pub(crate) fn report_interrupt_summary(
    stopped: bool,
    reporter: &dyn Reporter,
    memory: &MemorySink,
) {
    if !stopped {
        return;
    }
    if let Some(message) = interrupt_summary(&memory.selected(), &memory.finished()) {
        reporter.report(&Event::Progress {
            step: None,
            message,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collect(Mutex<Vec<Event>>);

    impl Reporter for Collect {
        fn report(&self, event: &Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    fn finished(pkg: &str, outcome: PackageOutcome) -> Event {
        Event::PackageFinished {
            pkg: pkg.into(),
            version: "1.0".into(),
            outcome,
        }
    }

    #[test]
    fn an_interrupted_build_counts_the_packages_not_started_and_in_flight() {
        let selected: Vec<String> = ["a", "b", "c", "d"].map(String::from).into();
        let done = [
            finished("a", PackageOutcome::Interrupted),
            finished("d", PackageOutcome::Built),
        ];
        assert_eq!(
            interrupt_summary(&selected, &done).as_deref(),
            Some(
                "interrupted: 2 package(s) not started, 1 in flight stopped; \
                 re-run the same command to continue"
            )
        );
    }

    #[test]
    fn nothing_left_unfinished_gives_no_summary() {
        let selected: Vec<String> = vec!["a".into()];
        assert_eq!(
            interrupt_summary(&selected, &[finished("a", PackageOutcome::Built)]),
            None
        );
        assert_eq!(interrupt_summary(&[], &[]), None);
    }

    #[test]
    fn a_package_selected_twice_is_counted_once() {
        let selected: Vec<String> = vec!["a".into(), "a".into(), "b".into()];
        let line = interrupt_summary(&selected, &[]).unwrap();
        assert!(line.starts_with("interrupted: 2 package(s) not started, 0 in flight"));
    }

    #[test]
    fn the_summary_is_emitted_as_one_progress_line() {
        let memory = MemorySink::new();
        memory.report(&Event::TargetSelected {
            pkg: "a".into(),
            version: "1.0".into(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        });
        let collect = Collect::default();
        report_interrupt_summary(true, &collect, &memory);
        let events = collect.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Progress {
                step: None,
                message,
            } => {
                assert_eq!(
                    message,
                    "interrupted: 1 package(s) not started, 0 in flight stopped; \
                     re-run the same command to continue"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_run_with_nothing_left_or_not_stopped_emits_nothing() {
        let memory = MemorySink::new();
        let collect = Collect::default();
        report_interrupt_summary(true, &collect, &memory);
        assert!(collect.0.lock().unwrap().is_empty());
        memory.report(&Event::TargetSelected {
            pkg: "a".into(),
            version: "1.0".into(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        });
        report_interrupt_summary(false, &collect, &memory);
        assert!(collect.0.lock().unwrap().is_empty());
    }
}
