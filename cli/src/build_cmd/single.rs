use super::*;

/// What a single-package build asks its caller to do next.
pub(super) enum Flow {
    /// Go round the outer loop again.
    Retry,
    /// The package is built.
    Done,
}

/// Builds the package of a source repository, which has no PyPI closure to
/// walk. Missing dependencies go back through the queue first.
pub(super) fn single_repo_build(b: Build<'_>) -> color_eyre::Result<Flow> {
    let Build {
        run_dir,
        reporter,
        pkg_reporter,
        rpm_name,
        version,
        branch,
        dep_drops,
        patch_requests,
        native_names,
        outdated_names,
        attempted_deps,
        pending_deps,
        config,
        ..
    } = b;
    let pkg_run = run_dir.pkg(&rpm_name);
    pkg_run.ensure()?;
    reporter.report(&Event::TargetSelected {
        pkg: rpm_name.to_string(),
        version: version.to_string(),
        reason: SelectionReason::ExplicitOverride,
        blocked_by: vec![],
        from_version: branch.and_then(|b| b.from_version.clone()),
    });
    let r = pkg_reporter(&rpm_name);
    let result = run_package(&config, &pkg_run, r.as_ref());
    let outcome = package_outcome(&result);
    r.report(&Event::PackageFinished {
        pkg: rpm_name.to_string(),
        version: version.to_string(),
        outcome,
    });
    match result {
        PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => {
            return Ok(Flow::Done);
        }
        PipelineResult::MissingDeps {
            deps,
            patches,
            native,
            outdated,
            ..
        } => {
            patch_requests.record(&patches);
            native_names.record(&native);
            outdated_names.record(&outdated);
            // Only queue deps we haven't tried yet; if every dep was
            // already attempted we cannot make progress. Compared by
            // `fuzzy_key`, not the raw string — see its doc comment.
            // A dep attempted earlier may have been built locally since;
            // it is available now, so drop it before judging progress.
            let remaining: Vec<String> = deps
                .iter()
                .filter(|d| {
                    built_locally(&config.state_root, &ap_build::osc::rpm_name(d)).is_none()
                })
                .cloned()
                .collect();
            let new_deps: Vec<String> = remaining
                .iter()
                .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
                .cloned()
                .collect();
            if new_deps.is_empty() && !remaining.is_empty() {
                // A target that still needs deps was not built, so
                // this is a failure, not success, when the same
                // unresolvable deps keep coming back. Surface
                // that as a failure, same shape as the frontier
                // path's no-progress give-up.
                let unbuildable = config.unbuildable_deps.join(", ");
                let failed = config.failed_deps.join(", ");
                return Err(color_eyre::eyre::eyre!(
                    "{rpm_name} needs {} but every one has already \
                     been attempted without success{}{}; cannot \
                     build. Why each one is out of reach:\n{}\n\
                     (no PyPI release AND no discoverable source repo is \
                     the usual answer — pass --source-url to \
                     point it at one).",
                    deps.join(", "),
                    if unbuildable.is_empty() {
                        String::new()
                    } else {
                        format!(" (unbuildable: {unbuildable})")
                    },
                    if failed.is_empty() {
                        String::new()
                    } else {
                        format!(" (failed-to-build: {failed})")
                    },
                    dep_drops.explain(&deps),
                ));
            }
            pending_deps.extend(new_deps);
            return Ok(Flow::Retry);
        }
        PipelineResult::Failed { reason, .. } => {
            return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
        }
    }
}
