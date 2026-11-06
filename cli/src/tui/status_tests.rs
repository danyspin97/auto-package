use super::*;

/// Every status a row can be in.
const ALL: [PkgStatus; 10] = [
    PkgStatus::Queued,
    PkgStatus::Running,
    PkgStatus::Between,
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
        let to = from.next(Input::StepFinished { notes: false });
        assert!(to == PkgStatus::Between, "{}", from.label());
    }
}

#[test]
fn the_notes_turn_keeps_the_package_done() {
    for from in ALL {
        for input in [
            Input::StepStarted { notes: true },
            Input::StepFinished { notes: true },
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
