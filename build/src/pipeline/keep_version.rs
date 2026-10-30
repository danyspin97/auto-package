//! Running the services while keeping the version, and staging the requested files.

use super::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;

/// `osc service ra` runs EVERY service of the checkout, `mode="disabled"` ones
/// included. An existing package's `_service` is the one OBS holds: a
/// `tar_scm` pinned to the old revision plus `set_version` rewrites `Version:`
/// back to the old release (pcp: 7.2.2 -> 5.2.5, so four builds compiled the
/// old tarball). The version is the pipeline's decision, so put it back.
pub(super) fn run_service_keeping_version(
    checkout_dir: &Path,
    spec_path: &Path,
    emit: Emit<'_>,
) -> Result<(), osc::Error> {
    // An obs_scm source is made by OBS at build time, and `osc service ra` would refuse
    // its `tar` service: nothing is run here, and the source is not missing for it.
    if source_made_at_build_time(checkout_dir) {
        return Ok(());
    }
    let before = std::fs::read_to_string(spec_path).unwrap_or_default();
    let result = osc::run_service(checkout_dir, emit);
    if let Ok(after) = std::fs::read_to_string(spec_path)
        && let Some(fixed) = keep_version_line(&before, &after)
    {
        ap_core::diag!("warning: osc service changed `Version:`; restoring the pipeline's version");
        write_or_warn(spec_path, &fixed);
    }
    result
}

/// `after` with its `Version:` line replaced by the one in `before`, when the
/// two differ; `None` when there is nothing to put back.
pub(super) fn keep_version_line(before: &str, after: &str) -> Option<String> {
    let version_line = |text: &str| {
        text.lines()
            .find(|l| l.trim_start().to_ascii_lowercase().starts_with("version:"))
            .map(str::to_string)
    };
    let (old, new) = (version_line(before)?, version_line(after)?);
    if old == new {
        return None;
    }
    Some(after.replacen(&new, &old, 1))
}

/// [`crate::stage::stage_files`] with the trees of this package's run.
pub(super) fn stage_for_config(
    config: &PipelineConfig,
    checkout_dir: &Path,
    requests: &[crate::stage::StageFile],
) -> (Vec<String>, Vec<String>) {
    let own_state = Workspace::find_existing(&config.state_root, &[&config.rpm_name])
        .map(|w| w.dir().to_path_buf())
        .unwrap_or_else(|| config.state_root.join(&config.rpm_name));
    let project_dir = config.work_dir.join(&config.project);
    let roots = crate::stage::StageRoots {
        own_state: &own_state,
        state_root: &config.state_root,
        project_dir: &project_dir,
        checkout: checkout_dir,
    };
    crate::stage::stage_files(requests, &roots)
}

/// Perform a turn's `stage_files` requests; the names copied. A refused entry
/// is reported and written to the notes, which the next turn reads.
pub(super) fn stage_requested_files(
    config: &PipelineConfig,
    checkout_dir: &Path,
    step: &StepDir,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
    notes_path: &Path,
    label: &str,
) -> Vec<String> {
    if outcome.stage_files.is_empty() {
        return Vec::new();
    }
    let (done, refused) = stage_for_config(config, checkout_dir, &outcome.stage_files);
    for message in &refused {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!("{}: {message}", config.rpm_name),
        });
    }
    if !refused.is_empty() {
        append_notes(
            notes_path.to_path_buf(),
            &format!(
                "\n## {label} — stage_files refused\n{}\n",
                refused.join("\n")
            ),
        );
    }
    for name in &done {
        reporter.report(&Event::Progress {
            step: Some(step.id),
            message: format!("{}: staged {name} into the checkout", config.rpm_name),
        });
    }
    done
}
