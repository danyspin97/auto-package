use super::*;

fn scope(targets: &[&str]) -> RunScope {
    RunScope {
        project: "home:foo:bar".to_string(),
        targets: targets.iter().map(|t| t.to_string()).collect(),
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
    };
    let line = header_line(&scope_labels(&long), counters(), 15);
    assert!(line.width() <= 15, "{line:?}");
    assert!(line.ends_with("..."), "{line:?}");
}

#[test]
fn the_sweep_counts_are_in_the_header_state() {
    let mut state = TuiState::new(scope(&[]));
    assert!(state.sweep.is_none());
    let progress = |checked, finished| Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked,
            to_update: 2,
            up_to_date: 30,
            skipped: 5,
            finished,
        },
    };
    state.apply(progress(37, false));
    assert_eq!(
        state.sweep.unwrap().label(),
        "checking: 37 (2 to update, 30 current, 5 skipped)"
    );
    state.apply(progress(37, true));
    assert!(state.sweep.unwrap().label().starts_with("checked 37"));
}

#[test]
fn without_a_sweep_the_counter_titles_have_no_sweep_part() {
    let state = TuiState::new(scope(&[]));
    for line in summary_counters(&state) {
        assert!(!line.contains("to update"), "{line:?}");
    }
}

#[test]
fn a_mid_width_header_still_shows_the_sweep_counts() {
    let mut state = TuiState::new(scope(&["pkg1"]));
    state.apply(Msg {
        pkg: None,
        event: Event::SweepProgress {
            checked: 37,
            to_update: 2,
            up_to_date: 30,
            skipped: 5,
            finished: false,
        },
    });
    let scopes = scope_labels(&state.scope);
    // Too narrow for the full title (it is over 100 columns with the sweep),
    // wide enough for the short ones.
    for width in 60..=100 {
        let line = header_line(&scopes, summary_counters(&state), width);
        assert!(!line.contains("Auto-Package"), "{width}: {line:?}");
        assert!(
            line.contains("37 checked, 2 to update"),
            "{width}: {line:?}"
        );
    }
}
