use super::*;

fn scope(targets: &[&str]) -> RunScope {
    RunScope {
        project: "home:foo:bar".to_string(),
        targets: targets.iter().map(|t| t.to_string()).collect(),
        jobs: 4,
    }
}

fn counters() -> Vec<String> {
    vec![
        " Auto-Package — 3/10 packages done, 7 pending (2 built) │ elapsed 12s │ cost $0.12 "
            .to_string(),
        " 3/10 done, 7 pending │ elapsed 12s │ $0.12 ".to_string(),
        " 3/10 done │ $0.12 ".to_string(),
    ]
}

#[test]
fn one_target_is_named_with_the_project_alone_as_the_last_resort() {
    assert_eq!(
        scope_labels(&scope(&["pkg1"])),
        [
            "project: home:foo:bar | target: pkg1",
            "project: home:foo:bar"
        ]
    );
}

#[test]
fn targets_that_do_not_fit_are_counted_from_the_end() {
    assert_eq!(
        scope_labels(&scope(&["pkg1", "pkg2", "pkg3"])),
        [
            "project: home:foo:bar | target: pkg1, pkg2, pkg3",
            "project: home:foo:bar | target: pkg1, pkg2 (+1)",
            "project: home:foo:bar | target: pkg1 (+2)",
            "project: home:foo:bar",
        ]
    );
}

#[test]
fn no_targets_means_the_whole_project() {
    assert_eq!(
        scope_labels(&scope(&[])),
        [
            "project: home:foo:bar | target: all packages",
            "project: home:foo:bar",
        ]
    );
}

#[test]
fn a_wide_header_shows_the_full_scope_before_the_full_counters() {
    let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 200);
    assert!(
        line.starts_with("project: home:foo:bar | target: pkg1, pkg2 │ Auto-Package"),
        "{line}"
    );
}

#[test]
fn a_narrow_header_drops_the_scope_before_it_shortens_the_counters() {
    let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 60);
    assert_eq!(line, " 3/10 done, 7 pending │ elapsed 12s │ $0.12 ");
    let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 40);
    assert_eq!(line, " 3/10 done │ $0.12 ");
}

#[test]
fn the_targets_are_dropped_before_the_project() {
    let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 70);
    assert_eq!(
        line,
        "project: home:foo:bar │ 3/10 done, 7 pending │ elapsed 12s │ $0.12 "
    );
}

#[test]
fn a_header_never_outgrows_its_width() {
    let scopes = scope_labels(&scope(&["a-long-package-name", "another-one", "third"]));
    // From 3 columns up: narrower than the "..." cut mark, nothing fits.
    for width in 3..=160 {
        let line = header_line(&scopes, counters(), width);
        assert!(line.width() <= width, "{width}: {line:?}");
    }
}

#[test]
fn a_project_too_long_for_any_width_is_cut_with_an_ellipsis() {
    let long = RunScope {
        project: "home:a-very-long-project-name:branches:openSUSE:Factory".to_string(),
        targets: vec!["pkg".to_string()],
        jobs: 0,
    };
    let line = header_line(&scope_labels(&long), counters(), 15);
    assert!(line.width() <= 15, "{line:?}");
    assert!(line.ends_with("..."), "{line:?}");
}

fn progress(checked: usize, finished: bool) -> Msg {
    Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked,
            to_update: 2,
            up_to_date: 30,
            skipped: 5,
            finished,
            package: None,
            total: None,
        },
    }
}

#[test]
fn the_sweep_counts_are_in_the_header_state() {
    let mut state = TuiState::new(scope(&[]));
    assert!(state.sweep.is_none());
    state.apply(progress(37, false));
    assert_eq!(
        state.sweep.unwrap().label(3),
        "checked 37, checking 3 · up-to-date 30 · needs update 2"
    );
    state.apply(progress(37, true));
    assert_eq!(
        state.sweep.unwrap().label(0),
        "checked 37 · up-to-date 30 · needs update 2"
    );
}

#[test]
fn without_a_sweep_the_counter_titles_have_no_sweep_part() {
    let state = TuiState::new(scope(&[]));
    for line in summary_counters(&state) {
        assert!(!line.contains("checked"), "{line:?}");
    }
}

/// A sweep that is done, with rows in every stage: 3 queued, 1 running, 1
/// built, 1 failed and 1 waiting.
fn state_in_every_stage(jobs: usize) -> TuiState {
    let mut state = TuiState::new(RunScope { jobs, ..scope(&[]) });
    state.apply(progress(37, true));
    for name in ["q1", "q2", "q3"] {
        state.pkg_mut(name);
    }
    let set = |state: &mut TuiState, name: &str, input: Input<'_>| {
        state.pkg_mut(name).advance(input);
    };
    set(&mut state, "run", Input::StepStarted { notes: false });
    let built = PackageOutcome::Built;
    let failed = PackageOutcome::Failed {
        reason: "x".into(),
        disk_full: false,
        remedy: None,
        note: None,
    };
    let missing = PackageOutcome::MissingDeps { deps: vec![] };
    for (name, outcome) in [("built", &built), ("failed", &failed), ("parked", &missing)] {
        set(
            &mut state,
            name,
            Input::Finished {
                outcome,
                disk_full: false,
            },
        );
    }
    state
}

#[test]
fn the_full_header_counts_each_stage_and_explains_the_queue() {
    let full = &summary_counters(&state_in_every_stage(4))[0];
    assert!(
        full.starts_with(
            " checked 37 · up-to-date 30 · needs update 2 · queued 3 (4 at a time) · running 1 · done 1 · failed 1 · waiting 1 │ elapsed 0s │ cost $0.00"
        ),
        "{full:?}"
    );
    // Without a --jobs limit there is nothing to explain.
    let unlimited = &summary_counters(&state_in_every_stage(0))[0];
    assert!(
        unlimited.contains("· queued 3 · running 1"),
        "{unlimited:?}"
    );
}

#[test]
fn a_mid_width_header_still_shows_the_sweep_counts() {
    let state = state_in_every_stage(4);
    let scopes = scope_labels(&state.scope);
    // Too narrow for the full title, wide enough for the short one.
    for width in 90..=135 {
        let line = header_line(&scopes, summary_counters(&state), width);
        assert!(!line.contains("elapsed"), "{width}: {line:?}");
        assert!(
            line.contains("checked 37 · current 30 · update 2 · queued 3 · run 1"),
            "{width}: {line:?}"
        );
    }
}

fn sweep_with_total(checked: usize, total: Option<usize>, finished: bool) -> SweepCounts {
    SweepCounts {
        checked,
        total,
        to_update: 2,
        up_to_date: 30,
        finished,
    }
}

#[test]
fn the_header_shows_the_total_once_it_is_known() {
    let known = sweep_with_total(37, Some(120), false);
    assert_eq!(
        known.label(3),
        "checked 37/120, checking 3 · up-to-date 30 · needs update 2"
    );
    assert_eq!(
        known.compact_label(3),
        "checked 37/120, checking 3 · current 30 · update 2"
    );
    let unknown = sweep_with_total(37, None, false);
    assert_eq!(
        unknown.label(3),
        "checked 37, checking 3 · up-to-date 30 · needs update 2"
    );
    assert_eq!(
        unknown.compact_label(3),
        "checked 37, checking 3 · current 30 · update 2"
    );
}

#[test]
fn a_finished_sweep_shows_no_checks_in_flight() {
    let done = sweep_with_total(120, Some(120), true);
    assert_eq!(
        done.label(0),
        "checked 120/120 · up-to-date 30 · needs update 2"
    );
    assert_eq!(
        done.compact_label(0),
        "checked 120/120 · current 30 · update 2"
    );
}

#[test]
fn the_header_counts_the_checks_in_flight_and_the_total() {
    let mut state = TuiState::new(scope(&[]));
    state.apply(Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked: 0,
            to_update: 0,
            up_to_date: 0,
            skipped: 0,
            finished: false,
            package: None,
            total: Some(120),
        },
    });
    state.apply(Msg {
        pkg: None,
        event: Event::SweepChecking {
            package: "newer".to_string(),
        },
    });
    let line = summary_counters(&state)[0].clone();
    assert!(
        line.contains("checked 0/120, checking 1 · up-to-date 0"),
        "{line:?}"
    );
}
