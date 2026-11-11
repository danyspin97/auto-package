//! The order of the package table and the selection in it.

use super::*;

fn finish(state: &mut TuiState, name: &str, outcome: PackageOutcome) {
    state.pkg_mut(name).advance(Input::Finished {
        outcome: &outcome,
        disk_full: false,
    });
}

/// Rows added in the order `built, queued, failed, running, parked`.
fn mixed() -> TuiState {
    let mut state = TuiState::new(RunScope::default());
    finish(&mut state, "built", PackageOutcome::Built);
    state.pkg_mut("queued");
    finish(
        &mut state,
        "failed",
        PackageOutcome::Failed {
            reason: "x".into(),
            disk_full: false,
            remedy: None,
            note: None,
        },
    );
    state
        .pkg_mut("running")
        .advance(Input::StepStarted { notes: false });
    finish(
        &mut state,
        "parked",
        PackageOutcome::MissingDeps { deps: vec![] },
    );
    state
}

fn names(state: &TuiState) -> Vec<&str> {
    state.visible_order().iter().map(|n| n.as_str()).collect()
}

#[test]
fn running_rows_come_first_then_queued_then_the_ended_ones() {
    assert_eq!(
        names(&mixed()),
        ["running", "queued", "parked", "failed", "built"]
    );
}

#[test]
fn rows_of_one_status_keep_the_order_they_appeared_in() {
    let mut state = TuiState::new(RunScope::default());
    for name in ["q3", "q1", "q2"] {
        state.pkg_mut(name);
    }
    assert_eq!(names(&state), ["q3", "q1", "q2"]);
}

#[test]
fn the_selection_follows_its_package_when_it_moves() {
    let mut state = mixed();
    let mut selected = Some("queued".to_string());
    assert_eq!(sync_selection(&state, &mut selected), Some(1));
    // The queued package starts: it sorts with the running ones now, ahead
    // of the one that started first, since it was found first.
    state
        .pkg_mut("queued")
        .advance(Input::StepStarted { notes: false });
    assert_eq!(sync_selection(&state, &mut selected), Some(0));
    assert_eq!(selected.as_deref(), Some("queued"));
    // It ends: the selection goes with it to the end of the table.
    finish(&mut state, "queued", PackageOutcome::Built);
    assert_eq!(sync_selection(&state, &mut selected), Some(4));
    assert_eq!(selected.as_deref(), Some("queued"));
}

#[test]
fn a_selected_row_that_is_hidden_hands_the_selection_to_the_first_row() {
    let mut state = mixed();
    let mut selected = Some("built".to_string());
    state.show_all = false;
    assert_eq!(sync_selection(&state, &mut selected), Some(0));
    assert_eq!(selected.as_deref(), Some("running"));
}

#[test]
fn the_arrow_keys_walk_the_sorted_rows() {
    let state = mixed();
    let mut selected = Some("running".to_string());
    select_next(&mut selected, &state);
    assert_eq!(selected.as_deref(), Some("queued"));
    select_next(&mut selected, &state);
    assert_eq!(selected.as_deref(), Some("parked"));
    select_prev(&mut selected, &state);
    assert_eq!(selected.as_deref(), Some("queued"));
}
