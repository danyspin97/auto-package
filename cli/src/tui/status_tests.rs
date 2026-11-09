use super::*;
use ap_core::sweep_event::SweepVerdict;

/// Every status a row can be in.
const ALL: [PkgStatus; 15] = [
    PkgStatus::Checking,
    PkgStatus::UpToDate,
    PkgStatus::NeedsUpdate,
    PkgStatus::CheckSkipped,
    PkgStatus::Queued,
    PkgStatus::Running,
    PkgStatus::Between,
    PkgStatus::Retrying,
    PkgStatus::Succeeded,
    PkgStatus::BuiltLocally,
    PkgStatus::AlreadyBuilt,
    PkgStatus::MissingDeps,
    PkgStatus::Failed,
    PkgStatus::NoSpace,
    PkgStatus::Skipped,
];

fn finished(outcome: PackageOutcome, disk_full: bool) -> PkgStatus {
    PkgStatus::Running.next(Input::Finished {
        outcome: &outcome,
        disk_full,
    })
}

fn failed(disk_full: bool) -> PackageOutcome {
    PackageOutcome::Failed {
        reason: "boom".into(),
        disk_full,
    }
}

#[test]
fn a_selection_queues_the_package_from_any_status() {
    for from in ALL {
        assert!(
            from.next(Input::Selected) == PkgStatus::Queued,
            "{}",
            from.label()
        );
    }
}

#[test]
fn a_step_runs_the_package_from_any_status() {
    for from in ALL {
        let to = from.next(Input::StepStarted { notes: false });
        assert!(to == PkgStatus::Running, "{}", from.label());
    }
}

#[test]
fn a_finished_step_leaves_the_package_between_steps() {
    for from in ALL {
        let to = from.next(Input::StepFinished {
            notes: false,
            failed: false,
        });
        assert!(to == PkgStatus::Between, "{}", from.label());
    }
}

#[test]
fn a_failed_step_is_a_retry_not_a_failed_package() {
    for from in ALL {
        let to = from.next(Input::StepFinished {
            notes: false,
            failed: true,
        });
        assert!(to == PkgStatus::Retrying, "{}", from.label());
    }
    // The package fails only once its build reports so.
    let outcome = failed(false);
    let to = PkgStatus::Retrying.next(Input::Finished {
        outcome: &outcome,
        disk_full: false,
    });
    assert!(to == PkgStatus::Failed);
}

#[test]
fn the_notes_turn_keeps_the_package_done() {
    for from in ALL {
        for input in [
            Input::StepStarted { notes: true },
            Input::StepFinished {
                notes: true,
                failed: false,
            },
            Input::StepFinished {
                notes: true,
                failed: true,
            },
        ] {
            assert!(from.next(input) == PkgStatus::Succeeded, "{}", from.label());
        }
    }
}

#[test]
fn each_outcome_ends_the_package_in_its_own_status() {
    assert!(finished(PackageOutcome::Built, false) == PkgStatus::Succeeded);
    assert!(finished(PackageOutcome::BuiltLocally, false) == PkgStatus::BuiltLocally);
    assert!(finished(PackageOutcome::AlreadyBuilt, false) == PkgStatus::AlreadyBuilt);
    let missing = PackageOutcome::MissingDeps { deps: vec![] };
    assert!(finished(missing, false) == PkgStatus::MissingDeps);
    assert!(finished(failed(false), false) == PkgStatus::Failed);
    let maintainer = PackageOutcome::NeedsMaintainer {
        reason: "gone".into(),
    };
    assert!(finished(maintainer, false) == PkgStatus::Failed);
    let skipped = PackageOutcome::Skipped {
        reason: "excluded".into(),
    };
    assert!(finished(skipped, false) == PkgStatus::Skipped);
}

#[test]
fn a_failure_is_out_of_space_when_the_outcome_or_an_earlier_step_says_so() {
    assert!(finished(failed(true), false) == PkgStatus::NoSpace);
    assert!(finished(failed(false), true) == PkgStatus::NoSpace);
    // Only a failure is blamed on the disk.
    assert!(finished(PackageOutcome::Built, true) == PkgStatus::Succeeded);
}

#[test]
fn only_a_package_being_worked_on_runs_its_clock() {
    for from in ALL {
        let runs = matches!(
            from,
            PkgStatus::Checking | PkgStatus::Running | PkgStatus::Between | PkgStatus::Retrying
        );
        assert_eq!(from.timer_runs(), runs, "{}", from.label());
    }
}

#[test]
fn a_clock_banks_its_time_when_it_stops_and_goes_on_from_there() {
    let mut clock = Clock::default();
    assert_eq!(clock.elapsed(), None);
    clock.follow(false);
    assert_eq!(clock.elapsed(), None, "a clock that never ran has no time");
    clock.follow(true);
    clock.running_since = Some(Instant::now() - Duration::from_secs(5));
    clock.follow(false);
    let banked = clock.elapsed().unwrap();
    assert!(banked >= Duration::from_secs(5));
    clock.follow(false);
    assert_eq!(
        clock.elapsed(),
        Some(banked),
        "a stopped clock does not move"
    );
    clock.follow(true);
    assert!(clock.elapsed().unwrap() >= banked);
}

#[test]
fn each_status_counts_in_one_header_group() {
    let counts = StageCounts::of(ALL.iter());
    assert_eq!(
        counts,
        StageCounts {
            queued: 1,
            running: 3,
            done: 3,
            failed: 2,
            waiting: 1,
            skipped: 2,
            checking: 1,
        }
    );
}

#[test]
fn a_running_row_says_its_try_once_the_pipeline_counts_one() {
    let mut pkg = PkgState::new("1".into());
    pkg.advance(Input::StepStarted { notes: false });
    assert_eq!(pkg.status_cell(), "running");
    (pkg.tries, pkg.max_tries) = (2, 3);
    assert_eq!(pkg.status_cell(), "running (try 2/3)");
    pkg.advance(Input::StepFinished {
        notes: false,
        failed: true,
    });
    assert_eq!(pkg.status_cell(), "running (try 2/3, retrying)");
    pkg.advance(Input::Finished {
        outcome: &failed(false),
        disk_full: false,
    });
    assert_eq!(pkg.status_cell(), "failed");
}

#[test]
fn a_parked_row_waits_upstream_and_a_skipped_one_says_why() {
    let mut state = TuiState::new(RunScope::default());
    let finish = |pkg: &str, outcome| Msg {
        pkg: None,
        event: Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "1".to_string(),
            outcome,
        },
    };
    state.apply(finish(
        "parked",
        PackageOutcome::MissingDeps {
            deps: vec!["x".into()],
        },
    ));
    assert_eq!(state.packages["parked"].status_cell(), "waiting-upstream");
    state.apply(finish(
        "skipped",
        PackageOutcome::Skipped {
            reason: "excluded on x86_64".into(),
        },
    ));
    let skipped = &state.packages["skipped"];
    assert_eq!(skipped.status_cell(), "skipped");
    assert_eq!(skipped.step_text(), "excluded on x86_64");
}

#[test]
fn a_check_result_moves_only_a_row_no_build_has_taken() {
    let current = SweepVerdict::UpToDate {
        version: "1".into(),
    };
    let update = SweepVerdict::Update {
        from: "1".into(),
        to: "2".into(),
    };
    let skip = SweepVerdict::Skipped {
        reason: "no spec".into(),
    };
    for from in ALL {
        for (verdict, checked) in [
            (&current, PkgStatus::UpToDate),
            (&update, PkgStatus::NeedsUpdate),
            (&skip, PkgStatus::CheckSkipped),
        ] {
            let to = from.next(Input::Checked(verdict));
            let want = if from.is_check_only() {
                checked
            } else {
                from.clone()
            };
            assert!(to == want, "{} -> {}", from.label(), to.label());
        }
    }
}

#[test]
fn only_the_check_statuses_are_check_only() {
    for from in ALL {
        let check = matches!(
            from,
            PkgStatus::Checking
                | PkgStatus::UpToDate
                | PkgStatus::NeedsUpdate
                | PkgStatus::CheckSkipped
        );
        assert_eq!(from.is_check_only(), check, "{}", from.label());
    }
}

#[test]
fn a_check_that_begins_takes_only_a_row_with_no_answer() {
    for from in ALL {
        let to = from.next(Input::Checking);
        let want = if from.is_check_only()
            && !matches!(
                from,
                PkgStatus::UpToDate | PkgStatus::NeedsUpdate | PkgStatus::CheckSkipped
            ) {
            PkgStatus::Checking
        } else {
            from.clone()
        };
        assert!(to == want, "{} -> {}", from.label(), to.label());
    }
}
