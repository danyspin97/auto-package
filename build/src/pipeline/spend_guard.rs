//! The per-package spend guard: whether an agent turn may start against the
//! package's budget.

/// How the reason of a stop at the budget begins (see [`spend_allows_turn`]).
pub(super) const BUDGET_REACHED: &str = "budget reached";

/// Whether a turn may start with `spent` already spent against `budget`: a
/// turn that would reach the budget (spent plus `last_turn`) does not start.
/// `Err` is the reason, for the package's skip or failure.
pub(super) fn spend_allows_turn(budget: f64, spent: f64, last_turn: f64) -> Result<(), String> {
    if spent + last_turn < budget {
        return Ok(());
    }
    Err(format!(
        "{BUDGET_REACHED}: ${spent:.2} spent of the ${budget:.2} budget, and the next turn is \
         expected to cost about ${last_turn:.2}, so no further agent turn runs \
         (--budget-usd changes the budget)"
    ))
}
