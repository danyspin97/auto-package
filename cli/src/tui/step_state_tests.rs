use super::*;
use ap_core::event::{SelectionReason, StepId};

fn msg(pkg: &str, event: Event) -> Msg {
    Msg {
        pkg: Some(pkg.to_string()),
        event,
    }
}

fn started(kind: StepKind) -> Event {
    Event::StepStarted {
        step: StepId(0),
        kind,
        attempt: 1,
        round: 0,
        detail: None,
    }
}

fn finished(outcome: StepOutcome, secs: u64) -> Event {
    Event::StepFinished {
        step: StepId(0),
        outcome,
        elapsed: Duration::from_secs(secs),
    }
}

fn package_finished(outcome: PackageOutcome) -> Event {
    Event::PackageFinished {
        pkg: "foo".to_string(),
        version: "1.0".to_string(),
        outcome,
    }
}

#[test]
fn a_waited_on_package_with_a_row_shows_its_status_not_a_second_entry() {
    let mut state = TuiState::new(RunScope::default());
    state.pkg_mut("python-logfire").status = PkgStatus::Running;
    let waiting = state.pkg_mut("python-mcp");
    waiting.status = PkgStatus::MissingDeps;
    waiting.waiting_on = vec!["logfire".to_string(), "typing-inspection".to_string()];
    assert_eq!(
        queued_lines(&state),
        ["python-mcp <- logfire (running), typing-inspection"]
    );
}

#[test]
fn missing_deps_sets_waiting_on_and_a_later_build_clears_it() {
    let mut state = TuiState::new(RunScope::default());
    let deps = ["b", "a", "b"].map(String::from).to_vec();
    state.apply(msg(
        "foo",
        package_finished(PackageOutcome::MissingDeps { deps }),
    ));
    assert_eq!(state.packages["foo"].waiting_on, ["a", "b"]);
    state.apply(msg("foo", package_finished(PackageOutcome::Built)));
    assert!(state.packages["foo"].waiting_on.is_empty());
}

#[test]
fn the_stage_counts_follow_each_row_status() {
    let mut state = TuiState::new(RunScope::default());
    state.pkg_mut("running");
    state.pkg_mut("built");
    state.pkg_mut("parked");
    state.pkg_mut("failed");
    assert_eq!(state.stage_counts().queued, 4);
    let fin = |pkg: &str, outcome| Event::PackageFinished {
        pkg: pkg.to_string(),
        version: "1".to_string(),
        outcome,
    };
    state.apply(msg("built", fin("built", PackageOutcome::Built)));
    state.apply(msg(
        "parked",
        fin("parked", PackageOutcome::MissingDeps { deps: vec![] }),
    ));
    state.apply(msg(
        "failed",
        fin(
            "failed",
            PackageOutcome::Failed {
                reason: "x".to_string(),
                disk_full: false,
            },
        ),
    ));
    let counts = state.stage_counts();
    assert_eq!(
        counts,
        StageCounts {
            queued: 1,
            done: 1,
            waiting: 1,
            failed: 1,
            ..StageCounts::default()
        }
    );
}

#[test]
fn queued_and_built_lines_follow_table_order() {
    let mut state = TuiState::new(RunScope::default());
    let fin = |pkg: &str, outcome| Event::PackageFinished {
        pkg: pkg.to_string(),
        version: "2.0".to_string(),
        outcome,
    };
    let missing = |deps: &[&str]| PackageOutcome::MissingDeps {
        deps: deps.iter().map(|d| d.to_string()).collect(),
    };
    state.apply(msg("w", fin("w", missing(&["y", "x"]))));
    state.apply(msg("v", fin("v", PackageOutcome::Built)));
    state.apply(msg("u", fin("u", missing(&[]))));
    state.apply(msg("t", fin("t", PackageOutcome::AlreadyBuilt)));
    assert_eq!(queued_lines(&state), ["w <- x, y", "u"]);
    assert_eq!(built_lines(&state), ["v 2.0", "t 2.0"]);
}

#[test]
fn run_level_progress_is_kept_for_the_event_log_before_any_package_has_a_row() {
    let mut state = TuiState::new(RunScope::default());
    for message in ["checking a (1/2)", "checking b (2/2)"] {
        state.apply(Msg {
            pkg: None,
            event: Event::Progress {
                step: None,
                message: message.to_string(),
            },
        });
    }
    let kept: Vec<&str> = state.run_log.iter().map(|l| l.message.as_str()).collect();
    assert_eq!(kept, ["checking a (1/2)", "checking b (2/2)"]);
    assert!(state.packages.is_empty());
}

#[test]
fn a_finished_step_between_steps_shows_its_label_not_a_dash() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg("foo", started(StepKind::Checkout)));
    state.apply(msg("foo", finished(StepOutcome::Ok, 2)));
    let pkg = &state.packages["foo"];
    assert!(pkg.status == PkgStatus::Between);
    let text = pkg.step_text();
    assert!(text.contains(StepKind::Checkout.label()));
    assert_ne!(text, "-");
}

#[test]
fn a_failed_step_then_failed_package_keeps_the_step_and_reason() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg("foo", started(StepKind::Checkout)));
    state.apply(msg("foo", finished(StepOutcome::Failed, 2)));
    state.apply(Msg {
        pkg: None,
        event: package_finished(PackageOutcome::Failed {
            reason: "boom".to_string(),
            disk_full: false,
        }),
    });
    let pkg = &state.packages["foo"];
    assert!(pkg.status == PkgStatus::Failed);
    let text = pkg.step_text();
    assert!(text.starts_with('✗'));
    assert!(text.contains(StepKind::Checkout.label()));
    assert_eq!(pkg.fail_reason.as_deref(), Some("boom"));
}

#[test]
fn a_built_package_has_no_step() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg("foo", started(StepKind::Checkout)));
    state.apply(msg("foo", finished(StepOutcome::Ok, 2)));
    state.apply(Msg {
        pkg: None,
        event: package_finished(PackageOutcome::Built),
    });
    let pkg = &state.packages["foo"];
    assert!(pkg.step.is_none());
    assert_eq!(pkg.step_text(), "-");
}

#[test]
fn a_new_attempt_clears_the_previous_failure() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg("foo", started(StepKind::Checkout)));
    state.apply(msg("foo", finished(StepOutcome::Failed, 2)));
    state.apply(Msg {
        pkg: None,
        event: package_finished(PackageOutcome::Failed {
            reason: "boom".to_string(),
            disk_full: false,
        }),
    });
    state.apply(Msg {
        pkg: None,
        event: Event::TargetSelected {
            pkg: "foo".to_string(),
            version: "1.0".to_string(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        },
    });
    let pkg = &state.packages["foo"];
    assert!(pkg.step.is_none());
    assert!(pkg.fail_reason.is_none());
}

fn target_selected() -> Msg {
    Msg {
        pkg: None,
        event: Event::TargetSelected {
            pkg: "foo".to_string(),
            version: "1.0".to_string(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        },
    }
}

#[test]
fn a_retry_resets_finished_and_tries() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg(
        "foo",
        Event::AgentPrompt {
            step: StepId(0),
            task: Task::FixAfterBuild {
                attempt: 2,
                max_attempts: 3,
            },
            prompt: String::new(),
            artifact: std::path::PathBuf::new(),
        },
    ));
    state.apply(Msg {
        pkg: None,
        event: package_finished(PackageOutcome::Failed {
            reason: "boom".to_string(),
            disk_full: false,
        }),
    });
    assert!(state.packages["foo"].finished);
    state.apply(target_selected());
    let pkg = &state.packages["foo"];
    assert!(!pkg.finished);
    assert_eq!((pkg.tries, pkg.max_tries), (0, 0));
}

#[test]
fn disk_full_only_classifies_the_failure_that_hit_it() {
    let mut state = TuiState::new(RunScope::default());
    state
        .packages
        .insert("foo".into(), PkgState::new("1.0".into()));
    state.packages.get_mut("foo").unwrap().disk_full_detected = true;
    state.apply(msg("foo", started(StepKind::Build)));
    state.apply(Msg {
        pkg: None,
        event: package_finished(PackageOutcome::Failed {
            reason: "boom".to_string(),
            disk_full: false,
        }),
    });
    assert!(state.packages["foo"].status == PkgStatus::Failed);
}

#[test]
fn a_repeated_step_shows_its_attempt_number() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg(
        "foo",
        Event::StepStarted {
            step: StepId(1),
            kind: StepKind::Build,
            attempt: 2,
            round: 2,
            detail: None,
        },
    ));
    let text = state.packages["foo"].step_text();
    assert!(text.contains("#2"), "got {text:?}");
}

#[test]
fn a_skipped_step_is_recorded_without_replacing_the_current_step() {
    let mut state = TuiState::new(RunScope::default());
    state.apply(msg("foo", started(StepKind::Checkout)));
    state.apply(msg(
        "foo",
        Event::StepSkipped {
            kind: StepKind::OscService,
            reason: SkipReason::NotRust,
        },
    ));
    let pkg = &state.packages["foo"];
    assert_eq!(pkg.skipped, [(StepKind::OscService, SkipReason::NotRust)]);
    assert_eq!(pkg.step.as_ref().unwrap().kind, StepKind::Checkout);
}

#[test]
fn a_package_time_runs_only_while_it_is_worked_on() {
    let mut pkg = PkgState::new("1.0".into());
    // Queued: no time yet, however long it waited.
    assert_eq!(pkg.time_cell(), "-");
    pkg.advance(Input::StepStarted { notes: false });
    pkg.clock.running_since = Some(Instant::now() - Duration::from_secs(90));
    assert!(pkg.clock.elapsed().unwrap() >= Duration::from_secs(90));
    // Finished: the time stops at the end.
    let outcome = PackageOutcome::Failed {
        reason: "boom".into(),
        disk_full: false,
    };
    pkg.advance(Input::Finished {
        outcome: &outcome,
        disk_full: false,
    });
    let frozen = pkg.clock.elapsed().unwrap();
    assert!(pkg.clock.running_since.is_none());
    assert!(frozen >= Duration::from_secs(90));
    assert_eq!(pkg.clock.elapsed(), Some(frozen));
    // Queued again for a retry: the time starts over.
    pkg.advance(Input::Selected);
    assert_eq!(pkg.time_cell(), "-");
}
