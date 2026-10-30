//! Row bookkeeping for the TUI: which requests settle, and which rows are withdrawn.

use crate::*;

/// The name a package is run, tracked and keyed under: its lowercase RPM guess
/// (see [`ap_build::osc::rpm_name`]). The spelling OBS lists a package under is
/// not used here: the pipeline resolves it from the own project's listing for
/// the OBS calls, and keeping one key per package keeps its workspace, state,
/// run directory and rows in one place (`python-APScheduler` and
/// `python-apscheduler` are the same package).
pub(crate) fn run_name(dep: &str) -> String {
    ap_build::osc::rpm_name(dep)
}

/// The selected targets that are neither running nor already finished in this
/// round: the ones the scheduler may start now, each with the name it is run
/// under (see [`run_name`]), which is how a running or finished package is
/// tracked.
pub(crate) fn unstarted_targets(
    targets: Vec<ap_build::pipeline::TargetInfo>,
    in_flight: &[String],
    done_this_round: &std::collections::HashSet<String>,
) -> Vec<(ap_build::pipeline::TargetInfo, String)> {
    targets
        .into_iter()
        .map(|t| {
            let rpm = run_name(&t.name);
            (t, rpm)
        })
        .filter(|(_, rpm)| !in_flight.contains(rpm) && !done_this_round.contains(rpm))
        .collect()
}

/// The first of `rpms` that `finished` reports as already built by this
/// invocation. The frontier must not start such a package again: its build is
/// done, so starting it again only repeats it, round after round. An explicit
/// `--target` names exactly one node and builds it as named, so it is never
/// reported.
pub(crate) fn finished_reselected<'a>(
    rpms: &'a [String],
    finished: impl Fn(&str) -> bool,
    explicit_target: bool,
) -> Option<&'a String> {
    if explicit_target {
        return None;
    }
    rpms.iter().find(|rpm| finished(rpm))
}

/// Forwards every event to `inner` and remembers whether a package was
/// selected to start, so a retry that returns `Ok` without starting anything
/// can be told apart from one that built something.
pub(crate) struct StartWatch {
    inner: Arc<dyn Reporter + Send + Sync>,
    started: std::sync::atomic::AtomicBool,
}

impl StartWatch {
    pub(crate) fn new(inner: Arc<dyn Reporter + Send + Sync>) -> Self {
        Self {
            inner,
            started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn started(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Reporter for StartWatch {
    fn report(&self, event: &Event) {
        if matches!(event, Event::TargetSelected { .. }) {
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.report(event);
    }
}

/// Wait for every package still running, so none is left running unobserved
/// when the build stops on an error. Their results are not used.
pub(crate) fn drain_in_flight(
    rx: &std::sync::mpsc::Receiver<(String, String, PipelineResult)>,
    in_flight: &mut Vec<String>,
) {
    while !in_flight.is_empty() {
        match rx.recv() {
            Ok((rpm_name, _, _)) => in_flight.retain(|r| *r != rpm_name),
            Err(_) => break,
        }
    }
}

/// A failed add request (the `n` key or a retry), kept until a later build of
/// its package settles it.
pub(crate) struct AddFailure {
    /// The name the request was made under (see `AddRequest`).
    pub(crate) label: String,
    pub(crate) message: String,
    /// How many `PackageFinished` events the run had reported when it failed.
    pub(crate) after: usize,
}

/// Whether `row` reported `PackageFinished` among `finished[from..]`. `finished`
/// is the run's record of `PackageFinished` events, in order.
pub(crate) fn finished_since(finished: &[Event], from: usize, row: &str) -> bool {
    finished
        .iter()
        .skip(from)
        .any(|e| matches!(e, Event::PackageFinished { pkg, .. } if pkg == row))
}

/// Drops the failures that a later build settled. A failure goes when a row it
/// was requested as (the request name, or the row's own RPM name) has its last
/// `PackageFinished` at or after the failure, and that outcome is built. `ends`
/// is the run's `PackageFinished` events in order, as (row, built).
pub(crate) fn withdraw_settled_failures(
    failures: &mut Vec<AddFailure>,
    ends: &[(String, bool)],
    label_of: impl Fn(&str) -> Option<String>,
) {
    let mut last: std::collections::HashMap<&str, (usize, bool)> = Default::default();
    for (at, (row, built)) in ends.iter().enumerate() {
        last.insert(row.as_str(), (at, *built));
    }
    failures.retain(|f| {
        !last.iter().any(|(row, (at, built))| {
            // A row has a recorded name when it was requested or built as a
            // dependency under one; otherwise the label names the package through
            // its RPM name (`foo` is `python-foo`), compared without case or
            // separators.
            let named = match label_of(row) {
                Some(label) => label == f.label,
                None => {
                    ap_build::osc::fuzzy_key(&ap_build::osc::rpm_name(&f.label))
                        == ap_build::osc::fuzzy_key(row)
                }
            };
            *built && *at >= f.after && (*row == f.label.as_str() || named)
        })
    });
}

/// The `PackageFinished` events in order, as (row, ended built).
pub(crate) fn row_ends(finished: &[Event]) -> Vec<(String, bool)> {
    finished
        .iter()
        .filter_map(|e| match e {
            Event::PackageFinished { pkg, outcome, .. } => Some((
                pkg.clone(),
                matches!(
                    outcome,
                    PackageOutcome::Built
                        | PackageOutcome::BuiltLocally
                        | PackageOutcome::AlreadyBuilt
                ),
            )),
            _ => None,
        })
        .collect()
}

/// [`withdraw_settled_failures`] over the events the run has reported so far.
pub(crate) fn withdraw_settled(
    failures: &mut Vec<AddFailure>,
    memory: &MemorySink,
    tui: &Option<tui::TuiController>,
) {
    withdraw_settled_failures(failures, &row_ends(&memory.finished()), |row| {
        tui.as_ref()
            .and_then(|t| t.label_of(row).or_else(|| t.origin_of(row)))
    });
}

/// The message of a retry that has no request name to resend. A dependency
/// row was never requested by name, and building its RPM name as a request
/// builds a different package (`python-foo` is `foo` on PyPI).
pub(crate) const NO_REQUEST_NAME: &str =
    "dependency rows cannot be retried from the TUI: rerun the build";

/// The name a TUI request is built under, and the row it retries (if it is a
/// retry). A retry takes the name its row was requested with, from `origin_of`;
/// a row with none gets no name, so nothing is built for it.
pub(crate) fn request_names(
    req: tui::AddRequest,
    origin_of: impl Fn(&str) -> Option<String>,
) -> (Option<String>, Option<String>) {
    match req {
        tui::AddRequest::New(name) => (Some(name), None),
        tui::AddRequest::Retry(row) => (origin_of(&row), Some(row)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ap_build::pipeline::{FinishedRoots, PipelineResult};

    /// A set in which `rpm` was finished by the invocation that owns it.
    fn finished_by_this_invocation(rpm: &str) -> FinishedRoots {
        let mut roots = FinishedRoots::default();
        roots.note(
            rpm,
            rpm,
            &PipelineResult::Built {
                rpm_name: rpm.to_string(),
            },
        );
        roots
    }

    #[test]
    fn a_root_this_invocation_finished_is_reported_when_selected_again() {
        let fresh = vec!["python-cffi".to_string(), "python-pywayland".to_string()];
        let mine = finished_by_this_invocation("python-pywayland");
        assert_eq!(
            finished_reselected(&fresh, |r| mine.contains(r), false).map(String::as_str),
            Some("python-pywayland")
        );
    }

    #[test]
    fn the_same_name_finished_by_another_invocation_is_not_reported() {
        let fresh = vec!["python-pywayland".to_string()];
        let other = finished_by_this_invocation("python-pywayland");
        let mine = FinishedRoots::default();
        assert!(finished_reselected(&fresh, |r| mine.contains(r), false).is_none());
        assert!(other.contains("python-pywayland"));
    }

    #[test]
    fn an_explicit_target_is_built_as_named_even_when_already_finished() {
        let fresh = vec!["python-pywayland".to_string()];
        let mine = finished_by_this_invocation("python-pywayland");
        assert!(finished_reselected(&fresh, |r| mine.contains(r), true).is_none());
    }

    #[test]
    fn a_selection_of_unfinished_packages_is_not_reported() {
        let fresh = vec!["python-cffi".to_string()];
        assert!(finished_reselected(&fresh, |_| false, false).is_none());
        assert!(finished_reselected(&[], |_| true, false).is_none());
    }
}
