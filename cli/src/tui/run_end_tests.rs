
use super::*;
use ap_core::event::SelectionReason;

fn finish(state: &mut TuiState, pkg: &str, outcome: PackageOutcome) {
    state.apply(Msg {
        pkg: None,
        event: Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "1".to_string(),
            outcome,
        },
    });
}

fn failed() -> PackageOutcome {
    PackageOutcome::Failed {
        reason: "x".to_string(),
        disk_full: false,
    }
}

#[test]
fn every_row_built_with_no_work_left_is_settled() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "built", PackageOutcome::Built);
    finish(&mut state, "already", PackageOutcome::AlreadyBuilt);
    assert!(run_is_done(&state, 0));
    assert!(run_is_over(&state, 0));
}

#[test]
fn a_failed_row_is_over_but_not_settled() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "built", PackageOutcome::Built);
    finish(&mut state, "failed", failed());
    assert!(!run_is_done(&state, 0));
    assert!(run_is_over(&state, 0));
}

#[test]
fn a_row_still_running_keeps_the_run_open() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "built", PackageOutcome::Built);
    state.pkg_mut("running").status = PkgStatus::Running;
    assert!(!run_is_done(&state, 0));
    assert!(!run_is_over(&state, 0));
}

#[test]
fn an_empty_run_is_neither_settled_nor_over_until_it_has_finished() {
    let state = TuiState::new(RunScope::default());
    assert!(!run_is_done(&state, 0));
    assert!(!run_is_over(&state, 0));
}

#[test]
fn a_retry_asked_for_keeps_the_run_open() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "foo", PackageOutcome::Built);
    state.pkg_mut("foo").retry_requested = true;
    assert!(!run_is_done(&state, 0));
    assert!(!run_is_over(&state, 0));
}

#[test]
fn a_built_row_that_is_re_queued_is_not_settled_until_it_builds_again() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "foo", PackageOutcome::Built);
    assert!(run_is_done(&state, 0));
    state.apply(Msg {
        pkg: None,
        event: Event::TargetSelected {
            pkg: "foo".to_string(),
            version: "1".to_string(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        },
    });
    assert!(!run_is_done(&state, 0));
    assert!(!run_is_over(&state, 0));
    finish(&mut state, "foo", PackageOutcome::Built);
    assert!(run_is_done(&state, 0));
}

#[test]
fn outstanding_work_keeps_the_run_open_even_when_every_row_is_settled() {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "built", PackageOutcome::Built);
    assert!(!run_is_done(&state, 1));
    assert!(!run_is_over(&state, 1));
}

#[test]
fn a_row_parked_on_missing_deps_is_not_settled_even_when_nothing_is_left() {
    let mut state = TuiState::new(RunScope::default());
    let deps = vec!["dep".to_string()];
    finish(&mut state, "parked", PackageOutcome::MissingDeps { deps });
    assert!(!run_is_done(&state, 0));
    assert!(run_is_over(&state, 0));
    assert!(!run_is_done(&state, 1));
}

#[test]
fn a_parked_row_stays_unsettled_once_its_dependency_is_built() {
    let mut state = TuiState::new(RunScope::default());
    let deps = vec!["dep".to_string()];
    finish(&mut state, "parked", PackageOutcome::MissingDeps { deps });
    finish(&mut state, "dep", PackageOutcome::Built);
    assert!(!run_is_done(&state, 0));
    assert!(run_is_over(&state, 0));
}

#[test]
fn the_hint_only_shows_once_the_run_is_over_and_not_settled() {
    assert_eq!(over_hint(false, false, true), None);
    assert_eq!(over_hint(false, true, true), None);
    assert_eq!(over_hint(true, true, true), None);
    assert!(over_hint(true, false, true).is_some());
}

#[test]
fn a_run_that_takes_no_requests_is_told_to_quit_not_to_add() {
    let hint = over_hint(true, false, false).unwrap();
    assert!(hint.contains("q to quit"));
    assert!(!hint.contains("add"));
}

#[test]
fn an_empty_run_that_succeeded_with_no_work_left_is_settled() {
    let mut state = TuiState::new(RunScope::default());
    state.run_outcome = Some(RunOutcome::Success);
    assert!(run_is_done(&state, 0));
}

#[test]
fn an_empty_run_with_work_still_outstanding_is_not_settled() {
    let mut state = TuiState::new(RunScope::default());
    state.run_outcome = Some(RunOutcome::Success);
    assert!(!run_is_done(&state, 1));
}

#[test]
fn an_empty_run_is_not_settled_before_its_outcome_or_after_a_failure() {
    let mut state = TuiState::new(RunScope::default());
    assert!(!run_is_done(&state, 0));
    state.run_outcome = Some(RunOutcome::Failed);
    assert!(!run_is_done(&state, 0));
}

#[test]
fn a_request_is_work_until_sent_and_is_released_if_the_run_is_gone() {
    let work = AtomicUsize::new(0);
    let (add_tx, add_rx) = channel::<AddRequest>();
    assert!(request_build(&add_tx, &work, AddRequest::New("foo".into())));
    assert_eq!(work.load(Ordering::SeqCst), 1);
    assert!(matches!(add_rx.recv().unwrap(), AddRequest::New(n) if n == "foo"));
    drop(add_rx);
    assert!(!request_build(
        &add_tx,
        &work,
        AddRequest::Retry("bar".into())
    ));
    assert_eq!(work.load(Ordering::SeqCst), 1);
}
