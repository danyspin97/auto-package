//! The `.changes` entry of an agent edit.

use super::*;
use crate::pipeline::licence::*;
use crate::pipeline::prompt_notes::*;

/// The text a turn's `.changes` bullet is made of: the changelog it gave, or,
/// when it gave none, its reasoning as one line.
pub(super) fn turn_bullet_source(outcome: &crate::agent::TurnOutcome) -> String {
    if outcome.changelog.trim().is_empty() {
        outcome
            .reasoning
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        outcome.changelog.clone()
    }
}

/// Adds the `.changes` entry of an agent edit: the turn's bullet, with the macros
/// and the version it wrote left out. The turn cannot run `osc vc` itself.
pub(super) fn add_edit_changes_entry(
    config: &PipelineConfig,
    checkout_dir: &Path,
    reporter: &dyn Reporter,
    step: &StepDir,
    bullet_source: &str,
) {
    warn_if_err(
        reporter,
        "adding the .changes entry for the agent's edit",
        osc::add_release_changes_entry(
            checkout_dir,
            &config.version,
            &without_own_version(
                &without_macros(
                    &changes_message(bullet_source),
                    &config.pkg_name,
                    &config.version,
                ),
                &config.pkg_name,
                &config.version,
            ),
            Emit::new(reporter, step.id),
        ),
    );
}
