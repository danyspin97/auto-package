//! The rows the update preparation dropped: they do not keep the run open,
//! and a failed one still fails the run through the exit, as before the rows.

use super::*;
use ap_core::event::SelectionReason;

fn selected(name: &str) -> Msg {
    Msg {
        pkg: None,
        event: Event::TargetSelected {
            pkg: name.to_string(),
            version: "2.0".to_string(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: Some("1.0".to_string()),
        },
    }
}

fn finished(name: &str, outcome: PackageOutcome) -> Msg {
    Msg {
        pkg: None,
        event: Event::PackageFinished {
            pkg: name.to_string(),
            version: "2.0".to_string(),
            outcome,
        },
    }
}

fn dropped(name: &str, outcome: PackageOutcome) -> Msg {
    Msg {
        pkg: None,
        event: Event::PackageDropped {
            pkg: name.to_string(),
            version: "2.0".to_string(),
            outcome,
        },
    }
}

fn skipped() -> PackageOutcome {
    PackageOutcome::Skipped {
        reason: "excluded by its spec".to_string(),
    }
}

fn failed() -> PackageOutcome {
    PackageOutcome::Failed {
        reason: "the branch failed".to_string(),
        disk_full: false,
        remedy: None,
    }
}

#[test]
fn a_dropped_skip_beside_built_rows_settles_the_run() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(selected("built"));
    state.apply(finished("built", PackageOutcome::Built));
    state.apply(selected("skipped"));
    state.apply(dropped("skipped", skipped()));
    assert!(state.packages["skipped"].status == PkgStatus::Skipped);
    assert!(run_is_done(&state, 0));
}

#[test]
fn a_dropped_failure_beside_built_rows_settles_and_the_exit_still_fails() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(selected("built"));
    state.apply(finished("built", PackageOutcome::Built));
    state.apply(selected("broken"));
    state.apply(dropped("broken", failed()));
    assert!(state.packages["broken"].status == PkgStatus::Failed);
    assert!(run_is_done(&state, 0));
    // The TUI closes on its own, so the failure reaches the exit from the
    // run's own list of failures, as it did before the row existed.
    let err = crate::tui_run_exit(
        run_is_done(&state, 0),
        None,
        Ok(()),
        vec!["1 package(s) failed to update".to_string()],
    )
    .unwrap_err();
    assert!(err.to_string().contains("failed to update"));
}

#[test]
fn a_failed_build_beside_built_rows_keeps_the_run_open() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(selected("built"));
    state.apply(finished("built", PackageOutcome::Built));
    state.apply(selected("broken"));
    state.apply(finished("broken", failed()));
    assert!(!run_is_done(&state, 0));
}

#[test]
fn a_build_skipped_by_obs_settles_the_run() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(selected("built"));
    state.apply(finished("built", PackageOutcome::Built));
    state.apply(selected("excluded"));
    state.apply(finished("excluded", skipped()));
    assert!(run_is_done(&state, 0));
}

#[test]
fn only_dropped_rows_settle_once_the_run_has_succeeded() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(dropped("skipped", skipped()));
    assert!(!run_is_done(&state, 0));
    state.run_outcome = Some(RunOutcome::Success);
    assert!(run_is_done(&state, 0));
}

#[test]
fn a_dropped_row_that_a_build_takes_over_waits_for_that_build() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(dropped("again", skipped()));
    state.apply(selected("again"));
    assert!(!run_is_done(&state, 0));
    state.apply(finished("again", PackageOutcome::Built));
    assert!(run_is_done(&state, 0));
}
