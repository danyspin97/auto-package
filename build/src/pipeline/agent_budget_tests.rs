//! The budget boundary of an agent turn: a turn that would reach the budget
//! does not start.

use crate::pipeline::spend_guard::spend_allows_turn;

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
