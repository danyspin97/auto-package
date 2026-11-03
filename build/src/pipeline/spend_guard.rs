//! The per-package spend guard: whether the next agent turn of a package may
//! start, from the budget (against what the last turn cost). The state lives in
//! the package's `RunCtx`, so it is kept for one package run and dropped with it.

/// What the package's agent turns cost, read by the guard before the next turn.
#[derive(Debug, Default)]
pub(super) struct TurnSignals {
    /// What the last agent turn cost, in dollars (0 before the first turn).
    pub(super) last_turn_usd: f64,
}

/// Records what a finished turn cost.
pub(super) fn record_turn(signals: &mut TurnSignals, outcome: &crate::agent::TurnOutcome) {
    signals.last_turn_usd = outcome.usage.cost_usd();
}

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

/// Whether a turn may start: the budget against the last turn's cost.
pub(super) fn turn_stop(budget: f64, spent: f64, signals: &TurnSignals) -> Result<(), String> {
    spend_allows_turn(budget, spent, signals.last_turn_usd)
}

/// The one check before an agent turn of a package starts, at every site that
/// starts one: [`turn_stop`] over the package's shared signals. `Err` is the
/// reason.
pub(super) fn can_start(
    budget: f64,
    spent: f64,
    signals: &std::sync::Mutex<TurnSignals>,
) -> Result<(), String> {
    let signals = signals.lock().unwrap_or_else(|e| e.into_inner());
    turn_stop(budget, spent, &signals)
}
