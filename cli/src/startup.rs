//! The start-up of a run on a project's work directory: the disk guard, and the
//! reconcile of what `state.json`, the checkouts and OBS say about each other.
//! A dry run reads only: it changes no state and no checkout.

use std::path::Path;

/// The local part of the start-up reconcile: a state the pipeline left unfinished
/// is settled, and stale leftovers and reviews are healed from the checkouts.
/// Nothing is changed when `dry_run` is set. Returns `(package, what was done)`.
pub(crate) fn heal_on_disk(work_dir: &Path, project: &str, dry_run: bool) -> Vec<(String, String)> {
    if dry_run {
        return Vec::new();
    }
    for (name, outcome) in ap_build::pipeline::reconcile_state(&work_dir.join("state")) {
        ap_core::diag!("note: {name} was left unfinished; now {outcome}");
    }
    ap_build::heal::checkouts(&work_dir.join("state"), &work_dir.join(project))
}

pub(crate) fn init_disk_guard(
    work_dir: &Path,
    min_free_gb: u64,
    project: &str,
    distro: &str,
    arch: &str,
    dry_run: bool,
) {
    ap_build::workspace::set_min_free_gb(min_free_gb);
    if let Some(warning) = ap_build::workspace::low_space_warning(work_dir) {
        ap_core::diag!("warning: {warning}");
    }
    let mut healed = heal_on_disk(work_dir, project, dry_run);
    // OBS is asked once, for the RPM fetch and the self-check (which only reads).
    let statuses = ap_build::osc::get_project_build_statuses(project, distro, arch).ok();
    // A package state.json calls unfinished while OBS shows it built gets its
    // RPMs fetched and its state corrected.
    if let Some(statuses) = statuses.as_ref().filter(|_| !dry_run) {
        let lower: std::collections::HashMap<_, _> = statuses
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let rpms_dir = work_dir.join("rpms");
        let _ = std::fs::create_dir_all(&rpms_dir);
        for (name, result) in ap_build::pipeline::reconcile_with_obs(
            &work_dir.join("state"),
            &work_dir.join(project),
            &rpms_dir,
            &lower,
            |checkout| ap_build::osc::has_user_changes(checkout, ap_build::proc::Emit::quiet()),
            |name| {
                ap_build::osc::download_built_rpms(
                    project,
                    name,
                    distro,
                    arch,
                    &rpms_dir,
                    ap_build::proc::Emit::quiet(),
                )
            },
        ) {
            match result {
                Ok(lint) => {
                    healed.push((name.clone(), "took OBS's RPMs".to_string()));
                    for warning in lint {
                        ap_core::diag!("note: {name}: {warning}");
                    }
                }
                Err(problem) => ap_core::diag!("note: {name}: {problem}"),
            }
        }
    }
    if let Some(line) = ap_build::heal::summary_line(&healed) {
        ap_core::diag!("{line}");
    }
    // The self-check: what the checkouts, state.json and OBS disagree about.
    let local_changes =
        |checkout: &Path| ap_build::osc::has_user_changes(checkout, ap_build::proc::Emit::quiet());
    for finding in ap_build::self_check::run(
        &work_dir.join("state"),
        &work_dir.join(project),
        || statuses,
        local_changes,
    ) {
        ap_core::diag!("warning: {finding}");
    }
}

#[cfg(test)]
mod tests;
