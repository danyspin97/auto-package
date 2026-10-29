//! The checkout stage: the preflight, the checkout paths and the checkout itself,
//! and the reconciliation of the RPMs with the state.

use super::*;
use crate::pipeline::linked::*;

/// Whether the spec at `path` still carries a TODO placeholder (convention 63).
/// An unreadable spec has none.
fn spec_has_placeholders(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| !crate::spec::todo_placeholders(&text).is_empty())
}

/// Name checks and the local-state fast path; opens the workspace.
pub(super) fn preflight<'a>(
    config: &'a PipelineConfig,
    pkg_run: &'a PkgRunDir,
    reporter: &'a dyn Reporter,
) -> Result<RunCtx<'a>, PipelineResult> {
    let rpm_name = config.rpm_name.clone();
    if let Err(reason) = validate_path_component("package name", &rpm_name)
        .and_then(|()| validate_path_component("project", &config.project))
    {
        return Err(PipelineResult::Failed {
            rpm_name,
            reason,
            kind: FailureKind::Other,
        });
    }
    let ws = match Workspace::open(&config.state_root, &rpm_name) {
        Ok(w) => w,
        Err(e) => {
            return Err(PipelineResult::Failed {
                rpm_name,
                reason: e.to_string(),
                kind: FailureKind::Other,
            });
        }
    };

    let prior = PkgState::load(&ws);
    // The OBS name comes from the project's own listing every run, so a
    // listing that changed is followed, and the state records what was
    // resolved. A listing that cannot be read is not a failure here: the
    // checkout stage lists the project again and reports it in its own step,
    // and until then the recorded name stands in (only local paths use it).
    let obs_name = match osc::try_list_project_packages(&config.project, Emit::run(reporter)) {
        Ok(own) => {
            let obs_name = resolve_obs_name(&rpm_name, &own);
            if prior.obs_name.as_deref() != Some(obs_name.as_str()) {
                update_state(&ws, reporter, |st| st.obs_name = Some(obs_name.clone()));
            }
            obs_name
        }
        Err(_) => prior.obs_name.clone().unwrap_or_else(|| rpm_name.clone()),
    };
    if let Err(reason) = validate_path_component("OBS package name", &obs_name) {
        return Err(PipelineResult::Failed {
            rpm_name,
            reason,
            kind: FailureKind::Other,
        });
    }
    let (checkout_dir, spec_path) = checkout_paths(&config.work_dir, &config.project, &obs_name);
    // A package whose checked-out spec is an unfinished skeleton is not done,
    // whatever an earlier build of it reported.
    let local_spec = spec_path.clone();
    let unfinished = spec_has_placeholders(&local_spec)
        || spec_has_published_defects(&local_spec, &config.rpms_dir)
        || wheel_policy_pending(&config.source_urls, &config.pkg_name, &local_spec);
    if !config.force && prior.is_built(&config.version) && !unfinished {
        return Err(PipelineResult::AlreadyBuilt { rpm_name });
    }
    Ok(RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        obs_name,
        checkout_dir,
        spec_path,
    })
}

/// Where the checkout of OBS package `obs_name` lives under `work_dir`, and its
/// spec file: `<work_dir>/<project>/<obs_name>/` and `<obs_name>.spec` inside it.
/// The checkout is named by the OBS name, which is case-sensitive.
pub(super) fn checkout_paths(work_dir: &Path, project: &str, obs_name: &str) -> (PathBuf, PathBuf) {
    let checkout_dir = work_dir.join(project).join(obs_name);
    let spec_path = checkout_dir.join(format!("{obs_name}.spec"));
    (checkout_dir, spec_path)
}

/// Step: Checkout, then the OBS-already-built fast path. Returns the package's
/// OBS build status for the later stages.
pub(super) fn checkout_stage(
    ctx: &RunCtx,
    st: &mut RunState,
) -> Result<Option<osc::BuildStatus>, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        obs_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    let obs_status: Option<osc::BuildStatus>;
    let has_local_changes: bool;
    {
        let step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::Checkout,
            None,
            step_attempts,
            rpm_name.clone()
        );

        let emit = Emit::new(reporter, step.id);
        // `osc::checkout` already runs `osc up` when the checkout dir
        // exists (and freshly `osc co`s when it doesn't), so this one
        // call brings the local tree up to date in both cases. The
        // previous flow also ran `osc::pull` right after, which is a
        // second `osc up` on the same directory — the server round-trip
        // was doubled on every single build. Removed.
        // A package the distro already ships is branched into the project and
        // updated from the distro's own recipe, not packaged from scratch.
        match branch_from_linked(config, rpm_name, emit) {
            Ok(Some(origin)) => reporter.report(&Event::Progress {
                step: Some(step.id),
                message: format!("branched {origin}/{rpm_name} into {}", config.project),
            }),
            Ok(None) => {}
            Err(BranchError::Renamed(why)) => reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!("not branching {rpm_name} from the distro: {why}"),
            }),
            // Falling through would create an empty package when OBS answers
            // 404 for the project, and the pipeline would then generate the
            // package from scratch and commit it into the project.
            Err(BranchError::Failed { reason, transient }) => {
                let id = step.id;
                step.finish(StepOutcome::Failed);
                let message = format!("branching {rpm_name} from the distro failed: {reason}");
                reporter.report(&Event::Warning {
                    step: Some(id),
                    message: message.clone(),
                });
                return Err(ctx.failed_text(transient, message));
            }
        }
        let result = osc::ensure_package(&config.project, obs_name, &config.work_dir, emit)
            .and_then(|()| {
                osc::checkout(&config.project, obs_name, &config.work_dir, emit).map(|_| ())
            });

        if let Err(e) = result {
            let id = step.id;
            step.finish(StepOutcome::Failed);
            reporter.report(&Event::Warning {
                step: Some(id),
                message: e.to_string(),
            });
            return Err(ctx.failed_osc(&e));
        }
        step.finish(StepOutcome::Ok);
        // Keep spec symlink fresh.
        let abs = spec_path
            .canonicalize()
            .unwrap_or_else(|_| spec_path.clone());
        warn_if_err(reporter, "updating the spec link", ws.link_spec(&abs));
        migrate_wheel_provenance(checkout_dir, ws, rpm_name, reporter);

        // A checkout dir is reused across runs (see `osc::checkout`), and
        // `osc up` never discards local uncommitted edits — so a spec that a
        // previous, interrupted run already bumped to a new version but
        // never `osc commit`-ed would otherwise make the AlreadyBuilt check
        // below believe OBS built a version it never actually saw.
        has_local_changes = osc::has_local_changes(checkout_dir, emit);

        // Fetch this package's current OBS build status once; reused below both
        // to grab the build log on failure and to skip the rest of the
        // pipeline when it already succeeded and nothing would change.
        let status = osc::get_build_status(&config.project, obs_name, &config.distro, &config.arch);
        // Without the status the build is handled as if OBS had not answered;
        // say so rather than discard the error.
        warn_if_err(
            reporter,
            &format!("{rpm_name}: reading the OBS build status"),
            status.as_ref().map(|_| ()),
        );
        obs_status = status.ok();

        // Fetch the previous build log if OBS reports a failed or
        // unresolvable build. Non-fatal — PreFix uses ws.build_log() if
        // present. No separate `osc results --verbose` fetch for the
        // Unresolvable case: the agent already inspects the spec and reports
        // what's missing via `unresolvable_packages`, so the OBS round-trip
        // would only duplicate work the agent will do anyway.
        if matches!(
            obs_status,
            Some(osc::BuildStatus::Failed | osc::BuildStatus::Unresolvable)
        ) {
            let log_path = ws.build_log();
            warn_if_err(
                reporter,
                "fetching the failed build log",
                osc::fetch_buildlog(
                    &config.project,
                    obs_name,
                    &config.distro,
                    &config.arch,
                    &log_path,
                    emit,
                ),
            );
        }
    }

    // OBS already has this exact version built successfully and nothing local
    // would change it — this covers the case local state.json (checked above)
    // doesn't know about, e.g. a different machine or a cleared state dir.
    // Re-checking-out and re-diffing a package that's already green wastes an
    // OBS build slot for no reason.
    if !config.force
        && !config.regen
        && spec_path.exists()
        && !has_local_changes
        && !spec_version_differs(spec_path, &config.version)
        && !spec_has_placeholders(spec_path)
        && !spec_has_published_defects(spec_path, &config.rpms_dir)
        && !wheel_policy_pending(&config.source_urls, &config.pkg_name, spec_path)
        && matches!(obs_status, Some(osc::BuildStatus::Succeeded))
        && reconcile_rpms(config, obs_name, ws, reporter)
    {
        update_state(ws, reporter, |state| {
            state.last_built_version = Some(config.version.clone());
            state.last_outcome = Some("success".to_string());
            state.last_failure_reason = None;
        });
        // This process may have cached `config.project`'s package listing
        // before learning (just now, via the fresh — not cached —
        // `get_build_status` call above) that OBS already has this one.
        osc::invalidate_project_packages_cache(&config.project);
        return Err(PipelineResult::AlreadyBuilt {
            rpm_name: rpm_name.clone(),
        });
    }
    Ok(obs_status)
}

/// Whether local state records a failure although OBS now reports the same
/// source as succeeded, so the RPMs must be fetched before success is recorded.
pub(super) fn state_needs_rpm_reconcile(prior: &PkgState) -> bool {
    prior.last_outcome.as_deref() != Some("success")
}

/// For a package OBS reports as succeeded: when local state disagrees (a
/// stale failure), download the RPMs first. Returns whether the package can
/// be recorded as built; `false` falls through to the normal pipeline.
fn reconcile_rpms(
    config: &PipelineConfig,
    obs_name: &str,
    ws: &Workspace,
    reporter: &dyn Reporter,
) -> bool {
    reconcile_rpms_with(config, obs_name, ws, reporter, || {
        osc::download_built_rpms(
            &config.project,
            obs_name,
            &config.distro,
            &config.arch,
            &config.rpms_dir,
            Emit::run(reporter),
        )
    })
}

/// [`reconcile_rpms`] with the download injected.
pub(super) fn reconcile_rpms_with(
    config: &PipelineConfig,
    obs_name: &str,
    ws: &Workspace,
    reporter: &dyn Reporter,
    download: impl FnOnce() -> Result<usize, osc::Error>,
) -> bool {
    if !state_needs_rpm_reconcile(&PkgState::load(ws)) {
        return true;
    }
    match download() {
        Ok(n) if n > 0 => {
            record_rpmlint(config, obs_name, ws, reporter);
            true
        }
        Ok(_) => {
            reporter.report(&Event::Warning {
                step: None,
                message: "OBS reports a success but no RPMs were downloaded; rebuilding".into(),
            });
            false
        }
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("OBS reports a success but fetching its RPMs failed: {e}"),
            });
            false
        }
    }
}
