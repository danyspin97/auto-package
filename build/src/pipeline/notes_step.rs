//! The package notes refresh, run as a visible step after a successful build.

use super::*;

/// Refresh the package's notes as a visible step. It runs after the build has
/// succeeded but before `PackageFinished`, so the TUI must show it: otherwise
/// the package reads as "running" with no step for as long as the model takes.
/// A failure only warns; the package stays built.
pub(super) fn package_notes_step(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    ws: &Workspace,
    reporter: &dyn Reporter,
) {
    // The notes turn is a model call and counts against the run's budget like
    // any other, so it is refused when its cap would not fit under the budget.
    // An unreadable state file would show no spend: the notes turn is skipped
    // rather than run against a budget it cannot see.
    let prior = match PkgState::try_load(ws) {
        Ok(prior) => prior,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("package notes not updated: cannot read the package state: {e}"),
            });
            return;
        }
    };
    let spent_this_run = spent_in_run(&prior, &config.run_id);
    // The notes turn is not started when, with what this run spent on the
    // package, its cap would reach the budget (the rule of every turn).
    if spend_allows_turn(
        config.budget_usd,
        spent_this_run,
        crate::package_notes::NOTES_CAP_USD,
    )
    .is_err()
    {
        reporter.report(&Event::Warning {
            step: None,
            message: format!(
                "package notes not updated: ${spent_this_run:.2} already spent in this run \
                 and the notes turn can cost up to ${:.2}, which would reach the ${:.2} budget",
                crate::package_notes::NOTES_CAP_USD,
                config.budget_usd
            ),
        });
        return;
    }
    let step = match StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::PackageNotes,
        None,
        &mut Attempts::default(),
    ) {
        Ok(step) => step,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("package notes not updated: {e}"),
            });
            return;
        }
    };
    // Reserve the cap before the call, so the spend is counted even if the
    // process dies during it. The reservation is corrected to the real cost once
    // the turn returns.
    add_run_spend(ws, &config.run_id, crate::package_notes::NOTES_CAP_USD);
    let run = crate::package_notes::update_after_build(
        ws,
        &config.rpm_name,
        &config.version,
        &config.project,
        &config.distro,
        &config.arch,
    );
    let correction = run.usage.cost_usd - crate::package_notes::NOTES_CAP_USD;
    if correction != 0.0 {
        add_run_spend(ws, &config.run_id, correction);
    }
    // Reported even when the update fails: a failed notes turn still cost money.
    if run.usage.cost_usd > 0.0 || run.usage.input_tokens + run.usage.output_tokens > 0 {
        reporter.report(&Event::NotesUsage {
            step: step.id,
            usage: run.usage.clone(),
        });
    }
    let outcome = match run.result {
        Ok(()) => StepOutcome::Ok,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!("package notes not updated: {e}"),
            });
            StepOutcome::Skipped
        }
    };
    step.finish(outcome);
}
