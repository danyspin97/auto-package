//! The per-package spend guard: whether the next agent turn of a package may
//! start, from the budget (against what the last turn cost) and the stop on a
//! repeated `no change` answer. The state lives in the package's `RunCtx`, so it
//! is kept for one package run and dropped with it.

use crate::agent::{Action, Task};
use crate::pipeline::render::brief;

/// What the package's agent turns cost and said, read by the guard before the
/// next turn.
#[derive(Debug, Default)]
pub(super) struct TurnSignals {
    /// What the last agent turn cost, in dollars (0 before the first turn).
    pub(super) last_turn_usd: f64,
    /// Per task kind: the `no change` answers of that kind in a row, with their
    /// reasoning. A turn of another kind does not touch them.
    streaks: Vec<Streak>,
}

/// The `no change` answers of one task kind in a row.
#[derive(Debug)]
struct Streak {
    task: Task,
    reasoning: String,
    count: u32,
}

/// `no change` answers in a row, of one task kind, with the same reasoning that
/// stop the package.
pub(super) const REPEAT_STOP: u32 = 2;

/// Records what a finished turn of `task` cost and whether it answered `no change`.
/// A PatchReview turn only sets the cost: it answers `no change` for every patch
/// it accepts, and two accepts in the same words are not a repeat.
pub(super) fn record_turn(
    signals: &mut TurnSignals,
    task: Task,
    outcome: &crate::agent::TurnOutcome,
) {
    signals.last_turn_usd = outcome.usage.cost_usd();
    if task == Task::PatchReview {
        return;
    }
    let at = signals.streaks.iter().position(|s| s.task == task);
    match &outcome.action {
        Action::NoChange => {
            let text = outcome.reasoning.trim().to_string();
            match at {
                Some(i) if signals.streaks[i].reasoning == text => signals.streaks[i].count += 1,
                Some(i) => {
                    signals.streaks[i].reasoning = text;
                    signals.streaks[i].count = 1;
                }
                None => signals.streaks.push(Streak {
                    task,
                    reasoning: text,
                    count: 1,
                }),
            }
        }
        _ => {
            if let Some(i) = at {
                signals.streaks.remove(i);
            }
        }
    }
}

/// How the reason of a stop at the budget begins (see [`spend_allows_turn`]).
pub(super) const BUDGET_REACHED: &str = "budget reached";

/// How the reason of a stop on repeated `no change` answers begins.
pub(super) const REPEATED_NO_CHANGE: &str = "repeated no change";

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

/// Whether a turn may start: the budget, then the repeated `no change` stop.
pub(super) fn turn_stop(budget: f64, spent: f64, signals: &TurnSignals) -> Result<(), String> {
    spend_allows_turn(budget, spent, signals.last_turn_usd)?;
    if let Some(s) = signals.streaks.iter().find(|s| s.count >= REPEAT_STOP) {
        let reasoning = if s.reasoning.is_empty() {
            "no reasoning given".to_string()
        } else {
            format!("the reasoning \"{}\"", brief(&s.reasoning))
        };
        return Err(format!(
            "{REPEATED_NO_CHANGE}: the last {} {:?} turns answered `no change` with the same \
             reasoning ({reasoning}); the package is stopped, the next {:?} turn would repeat it",
            s.count, s.task, s.task
        ));
    }
    Ok(())
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
