//! The per-package spend guard: the budget boundaries with the last turn's cost,
//! and the stop on a repeated `no change` answer.

use crate::agent::Task;
use crate::pipeline::spend_guard::{
    BUDGET_REACHED, REPEATED_NO_CHANGE, TurnSignals, can_start, record_turn, spend_allows_turn,
    turn_stop,
};
use crate::pipeline::submodule_wheel::pipeline_wheel_outcome;

fn turn(action: crate::agent::Action, reasoning: &str) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.reasoning = reasoning.into();
    out.evidence = Vec::new();
    out
}

#[test]
fn a_turn_starts_just_below_the_budget_and_not_at_or_over_it() {
    // Budget $3: spent $1.99 and a $0.99 turn reach $2.98, so the turn starts.
    assert!(spend_allows_turn(3.0, 1.99, 0.99).is_ok());
    // Spent $2.00 and a $1.00 turn reach exactly the budget: no turn starts.
    assert!(spend_allows_turn(3.0, 2.0, 1.0).is_err());
    // Over the budget: no turn starts.
    assert!(spend_allows_turn(3.0, 2.5, 1.0).is_err());
    // Nothing spent and no turn yet: the first turn starts.
    assert!(spend_allows_turn(3.0, 0.0, 0.0).is_ok());
    // Spent already at the budget, with no last turn: stopped.
    assert!(spend_allows_turn(3.0, 3.0, 0.0).is_err());
}

#[test]
fn the_reason_names_the_spend_the_budget_and_the_flag() {
    let why = spend_allows_turn(3.0, 2.5, 1.0).unwrap_err();
    assert!(why.contains("$2.50 spent of the $3.00 budget"), "{why}");
    assert!(why.contains("about $1.00"), "{why}");
    assert!(why.contains("--budget-usd"), "{why}");
}

#[test]
fn the_review_and_the_notes_turn_need_their_cap_in_room_below_the_budget() {
    use crate::pipeline::build_round::POST_REVIEW_MIN_BUDGET_USD;
    // $2.49 spent of $3: the review's $0.50 stays below the budget.
    assert!(spend_allows_turn(3.0, 2.49, POST_REVIEW_MIN_BUDGET_USD).is_ok());
    // $2.50 spent: the review's $0.50 would reach it, which is refused.
    assert!(spend_allows_turn(3.0, 2.5, POST_REVIEW_MIN_BUDGET_USD).is_err());
    // The notes turn's cap reaching the budget exactly is refused too.
    let cap = crate::package_notes::NOTES_CAP_USD;
    assert!(spend_allows_turn(3.0 + cap, 3.0, cap).is_err());
    assert!(spend_allows_turn(3.01 + cap, 3.0, cap).is_ok());
}

#[test]
fn the_last_turns_cost_counts_against_the_budget() {
    let mut signals = TurnSignals::default();
    signals.last_turn_usd = 1.0;
    assert!(turn_stop(3.0, 1.5, &signals).is_ok());
    assert!(turn_stop(3.0, 2.0, &signals).is_err());
}

#[test]
fn a_fresh_package_run_starts_with_no_last_turn() {
    // The state is per package run: what one run's turn cost does not count in the next.
    let mut expensive = pipeline_wheel_outcome("x");
    expensive.usage.cli_cost_usd = Some(2.0);
    let ran = std::sync::Mutex::new(TurnSignals::default());
    record_turn(&mut ran.lock().unwrap(), Task::Review, &expensive);
    assert!(can_start(3.0, 1.0, &ran).is_err());
    let next = std::sync::Mutex::new(TurnSignals::default());
    assert!(can_start(3.0, 1.0, &next).is_ok());
}

#[test]
fn a_first_no_change_never_stops_whatever_its_reasoning() {
    for reasoning in ["", "the log is a network timeout"] {
        let mut signals = TurnSignals::default();
        record_turn(
            &mut signals,
            Task::Review,
            &turn(crate::agent::Action::NoChange, reasoning),
        );
        assert!(turn_stop(3.0, 0.0, &signals).is_ok(), "{reasoning:?}");
    }
}

#[test]
fn the_same_no_change_reasoning_twice_in_a_row_stops_and_names_it() {
    let said = turn(crate::agent::Action::NoChange, "  the log is a timeout ");
    let mut signals = TurnSignals::default();
    record_turn(&mut signals, Task::Review, &said);
    record_turn(&mut signals, Task::Review, &said);
    let why = turn_stop(3.0, 0.0, &signals).unwrap_err();
    assert!(why.contains("the log is a timeout"), "{why}");
    assert!(why.contains("`no change`"), "{why}");
    assert!(!why.contains("unexplained"), "{why}");
}

#[test]
fn different_no_change_reasons_or_an_edit_in_between_do_not_stop() {
    let mut signals = TurnSignals::default();
    record_turn(
        &mut signals,
        Task::Review,
        &turn(crate::agent::Action::NoChange, "one"),
    );
    record_turn(
        &mut signals,
        Task::Review,
        &turn(crate::agent::Action::NoChange, "two"),
    );
    assert!(turn_stop(3.0, 0.0, &signals).is_ok());
    record_turn(
        &mut signals,
        Task::Review,
        &turn(crate::agent::Action::NoChange, "two"),
    );
    assert!(turn_stop(3.0, 0.0, &signals).is_err());
    // An edit clears the run of repeats.
    record_turn(
        &mut signals,
        Task::Review,
        &turn(crate::agent::Action::Edited, ""),
    );
    assert!(turn_stop(3.0, 0.0, &signals).is_ok());
}

#[test]
fn a_repeat_stop_is_named_as_such_and_not_as_the_budget() {
    let said = turn(crate::agent::Action::NoChange, "same reason");
    let mut signals = TurnSignals::default();
    record_turn(&mut signals, Task::Review, &said);
    record_turn(&mut signals, Task::Review, &said);
    let why = turn_stop(3.0, 0.0, &signals).unwrap_err();
    assert!(why.starts_with(REPEATED_NO_CHANGE), "{why}");
    assert!(!why.starts_with(BUDGET_REACHED), "{why}");
}

#[test]
fn the_repeat_count_is_per_task_kind() {
    // A `no change` of one kind and one of another do not add up to a repeat.
    let said = turn(crate::agent::Action::NoChange, "same reason");
    let mut signals = TurnSignals::default();
    record_turn(&mut signals, Task::Review, &said);
    record_turn(&mut signals, Task::FixAfterBuild, &said);
    assert!(turn_stop(3.0, 0.0, &signals).is_ok());
    record_turn(&mut signals, Task::Review, &said);
    let why = turn_stop(3.0, 0.0, &signals).unwrap_err();
    assert!(why.contains("Review"), "{why}");
}
