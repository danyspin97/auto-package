//! The rows of the packages the sweep checked.

use super::*;
use ap_core::event::SelectionReason;
use ap_core::sweep_event::{SweepVerdict, SweptPackage};

fn checked(name: &str, verdict: SweepVerdict) -> Msg {
    Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked: 1,
            to_update: 0,
            up_to_date: 0,
            skipped: 0,
            finished: false,
            package: Some(SweptPackage {
                name: name.to_string(),
                obs_name: String::new(),
                verdict,
            }),
            total: None,
        },
    }
}

fn checking(name: &str) -> Msg {
    Msg {
        pkg: None,
        event: Event::SweepChecking {
            package: name.to_string(),
        },
    }
}

/// The sweep's last report: every check it began has its answer by now.
fn finished() -> Msg {
    Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked: 2,
            to_update: 1,
            up_to_date: 0,
            skipped: 0,
            finished: true,
            package: None,
            total: Some(2),
        },
    }
}

fn update() -> SweepVerdict {
    SweepVerdict::Update {
        from: "1.0".into(),
        to: "2.0".into(),
    }
}

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

#[test]
fn every_checked_package_gets_a_row_with_what_its_check_found() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checked(
        "current",
        SweepVerdict::UpToDate {
            version: "3.1".into(),
        },
    ));
    state.apply(checked("newer", update()));
    state.apply(checked(
        "odd",
        SweepVerdict::Skipped {
            reason: "no spec".into(),
        },
    ));
    assert_eq!(state.order, ["current", "newer", "odd"]);
    let current = &state.packages["current"];
    assert_eq!(current.status_cell(), "up-to-date");
    assert_eq!(current.version_label(), "3.1");
    assert_eq!(current.time_cell(), "-");
    let newer = &state.packages["newer"];
    assert_eq!(newer.status_cell(), "needs-update");
    assert_eq!(newer.version_label(), "1.0 → 2.0");
    let odd = &state.packages["odd"];
    assert_eq!(odd.status_cell(), "skipped");
    assert_eq!(odd.step_text(), "no spec");
    // Only the skip counts in the build stages; the rest are check results.
    assert_eq!(
        state.stage_counts(),
        StageCounts {
            skipped: 1,
            ..StageCounts::default()
        }
    );
}

#[test]
fn a_queued_update_leaves_the_check_behind_and_a_late_check_does_not_undo_it() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checked("newer", update()));
    state.apply(selected("newer"));
    assert!(state.packages["newer"].status == PkgStatus::Queued);
    state.apply(checked("newer", update()));
    assert!(state.packages["newer"].status == PkgStatus::Queued);
    assert_eq!(state.stage_counts().queued, 1);
}

#[test]
fn check_only_rows_do_not_keep_the_run_open() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checked(
        "current",
        SweepVerdict::UpToDate {
            version: "1".into(),
        },
    ));
    state.apply(checked(
        "odd",
        SweepVerdict::Skipped {
            reason: "no spec".into(),
        },
    ));
    // Nothing was queued: the run ends as one with no rows does.
    assert!(!run_is_done(&state, 0));
    assert!(!run_is_over(&state, 0));
    state.run_outcome = Some(RunOutcome::Success);
    assert!(run_is_done(&state, 0));
    // A built package settles the run beside them.
    state.apply(selected("newer"));
    state.apply(Msg {
        pkg: None,
        event: Event::PackageFinished {
            pkg: "newer".to_string(),
            version: "2.0".to_string(),
            outcome: PackageOutcome::Built,
        },
    });
    state.run_outcome = None;
    assert!(run_is_done(&state, 0));
    assert!(run_is_over(&state, 0));
}

#[test]
fn hiding_the_settled_rows_hides_the_up_to_date_ones_too() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checked(
        "current",
        SweepVerdict::UpToDate {
            version: "1".into(),
        },
    ));
    state.apply(checked("newer", update()));
    assert_eq!(state.visible_order().len(), 2);
    state.show_all = false;
    assert_eq!(state.visible_order(), [&"newer".to_string()]);
}

#[test]
fn a_started_check_shows_checking_until_its_answer_arrives() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checking("current"));
    state.apply(checking("newer"));
    assert_eq!(state.order, ["current", "newer"]);
    assert_eq!(state.packages["current"].status_cell(), "checking");
    // A check in flight is counted as a check, not as a build stage.
    assert_eq!(
        state.stage_counts(),
        StageCounts {
            checking: 2,
            ..StageCounts::default()
        }
    );
    state.apply(checked(
        "current",
        SweepVerdict::UpToDate {
            version: "3.1".into(),
        },
    ));
    assert_eq!(state.packages["current"].status_cell(), "up-to-date");
    assert_eq!(state.packages["current"].version_label(), "3.1");
    assert_eq!(state.packages["newer"].status_cell(), "checking");
    state.apply(checked("newer", update()));
    assert_eq!(state.packages["newer"].status_cell(), "needs-update");
    state.apply(checking("odd"));
    state.apply(checked(
        "odd",
        SweepVerdict::Skipped {
            reason: "no spec".into(),
        },
    ));
    assert_eq!(state.packages["odd"].status_cell(), "skipped");
}

#[test]
fn a_check_that_begins_does_not_undo_an_answer_or_a_build() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checked(
        "current",
        SweepVerdict::UpToDate {
            version: "1".into(),
        },
    ));
    state.apply(checking("current"));
    assert_eq!(state.packages["current"].status_cell(), "up-to-date");
    state.apply(selected("newer"));
    state.apply(checking("newer"));
    assert!(state.packages["newer"].status == PkgStatus::Queued);
}

#[test]
fn a_row_begun_under_its_obs_name_moves_to_the_name_its_build_has() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checking("python-APScheduler"));
    assert_eq!(state.order, ["python-APScheduler"]);
    state.apply(Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked: 1,
            to_update: 1,
            up_to_date: 0,
            skipped: 0,
            finished: false,
            package: Some(SweptPackage {
                name: "python-apscheduler".into(),
                obs_name: "python-APScheduler".into(),
                verdict: update(),
            }),
            total: None,
        },
    });
    assert_eq!(state.order, ["python-apscheduler"]);
    assert!(!state.packages.contains_key("python-APScheduler"));
    assert_eq!(
        state.packages["python-apscheduler"].status_cell(),
        "needs-update"
    );
}

#[test]
fn a_finished_sweep_removes_the_checks_it_never_answered() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(checking("answered"));
    state.apply(checking("unanswered"));
    state.apply(checked("answered", update()));
    assert_eq!(state.stage_counts().checking, 1);
    // Not finished yet: a check still in flight stays.
    state.apply(checked(
        "answered",
        SweepVerdict::UpToDate {
            version: "3.1".into(),
        },
    ));
    assert!(state.packages.contains_key("unanswered"));
    state.apply(finished());
    assert!(!state.packages.contains_key("unanswered"));
    assert_eq!(state.order, ["answered"]);
    assert_eq!(state.stage_counts().checking, 0);
    assert_eq!(state.packages["answered"].status_cell(), "up-to-date");
}
