//! The spend the agent turns charge to a package and the run, and the daily
//! cap they are checked against. A failed model call is charged here too.

use super::*;
use crate::pipeline::archive::*;

/// Charge a failed model call's cost to the package and the run as soon as the
/// call returns. A retry overwrites the error it follows, so the cost cannot
/// wait for the final result: that would drop the first call's spend.
pub(super) fn charge_failed_call(
    work_dir: &Path,
    ws: &Workspace,
    run_id: &str,
    spent_usd: &mut f64,
    result: &Result<crate::agent::TurnOutcome, crate::agent::Error>,
    reporter: &dyn Reporter,
    step: ap_core::event::StepId,
) {
    let Err(e) = result else {
        return;
    };
    if let Some(usage) = e.usage() {
        *spent_usd += usage.cost_usd();
        // Saved now, not with the next successful turn: a package that
        // stops here would otherwise lose the spend in state.json.
        add_run_spend(ws, run_id, usage.cost_usd());
        record_daily_spend(work_dir, usage.cost_usd());
        if usage.cost_usd() > 0.0 || usage.input_tokens + usage.output_tokens > 0 {
            reporter.report(&Event::FailedTurnUsage {
                step,
                usage: ap_core::event::Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cost_usd: usage.cost_usd(),
                },
            });
        }
    }
}

/// Add a finished turn's cost to the work directory's daily ledger. Spend
/// already incurred cannot be refused, so a ledger that cannot be written is
/// only reported; the next turn then fails closed in `daily_cap_check`.
pub(crate) fn record_daily_spend(work_dir: &Path, usd: f64) {
    let dir = ap_core::limits::ledger_dir().unwrap_or_else(|| work_dir.to_path_buf());
    if let Err(e) = ap_core::ledger::record(&dir, usd) {
        ap_core::diag!("  cannot record ${usd:.2} in the spend ledger: {e}");
    }
}

/// Refuse a turn that would take the day's spend past `--max-total-usd`, or
/// when the ledger cannot be read or locked.
pub(crate) fn daily_cap_check(work_dir: &Path) -> Result<(), String> {
    let dir = ap_core::limits::ledger_dir().unwrap_or_else(|| work_dir.to_path_buf());
    ap_core::ledger::check(
        &dir,
        ap_core::limits::ESTIMATED_TURN_USD,
        ap_core::limits::max_total_usd(),
    )
}
