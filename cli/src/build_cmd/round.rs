use super::dep_config::as_dependency;
use super::*;

/// The state of one round of the frontier: what the round needs from the
/// build beyond its queue.
pub(super) struct Round<'a> {
    pub(super) name: &'a str,
    pub(super) explicit_target: Option<&'a str>,
    pub(super) pinned_version: Option<&'a str>,
    pub(super) tx: &'a std::sync::mpsc::Sender<(String, String, PipelineResult)>,
    pub(super) rx: &'a std::sync::mpsc::Receiver<(String, String, PipelineResult)>,
    pub(super) in_flight: &'a mut Vec<String>,
    pub(super) fail_counts: &'a mut std::collections::HashMap<String, usize>,
    pub(super) repeats: &'a mut RepeatMemory,
    pub(super) rate_limited_counts: &'a mut std::collections::HashMap<String, usize>,
    pub(super) give_ups: &'a mut std::collections::BTreeMap<String, String>,
    pub(super) permanently_failed: &'a mut std::collections::HashSet<String>,
    /// The requested root, once this invocation has finished it.
    pub(super) finished_roots: &'a mut FinishedRoots,
    pub(super) max_target_retries: usize,
}

/// What a frontier round leaves for the rest of the build.
pub(super) struct Spawned {
    pub(super) spawned_any: bool,
    pub(super) batch_pending: Vec<String>,
    pub(super) batch_failures: Vec<(String, String)>,
}

/// Starts the packages of the frontier as their dependencies finish, and
/// settles each one that finishes, until the frontier has nothing left to start.
pub(super) fn frontier_stream(b: Build<'_>, r: Round<'_>) -> color_eyre::Result<Spawned> {
    let Build {
        project,
        branch,
        reporter,
        pkg_reporter,
        tui_ctrl,
        sem,
        run_dir,
        dep_drops,
        patch_requests,
        native_names,
        outdated_names,
        attempted_deps,
        config,
        ..
    } = b;
    let Round {
        name,
        explicit_target,
        pinned_version,
        tx,
        rx,
        in_flight,
        fail_counts,
        repeats,
        rate_limited_counts,
        give_ups,
        permanently_failed,
        finished_roots,
        max_target_retries,
    } = r;
    // Select the frontier and build it as a stream: a package starts as soon
    // as everything it needs has been built, rather than when the whole
    // round has finished. Each finished package is handled below and the
    // frontier is selected again. A package that finished in this round is
    // not started again until the next round, so a retry keeps the
    // round-level pacing.
    //
    // `config.force` is passed down so `--force` (and `update`, which
    // always sets it) can override the "OBS says it's already green"
    // resolvability gate for the root -- see `select_target`'s doc
    // comment. Without this, the gate would skip the package before
    // `run_package`'s own force check could ever fire.
    let mut done_this_round: std::collections::HashSet<String> = Default::default();
    let mut spawned_any = false;
    let mut batch_pending: Vec<String> = Vec::new();
    let mut batch_failures: Vec<(String, String)> = Vec::new();
    loop {
        if ap_build::proc::shutting_down() {
            drain_in_flight(&rx, in_flight);
            return Err(color_eyre::eyre::eyre!("interrupted"));
        }
        // The package names OBS lists are what builds and tracking use, so
        // the listing is read with the frontier: a failure of either ends
        // the build, after the packages already running have finished.
        let selected = select_target(
            name,
            explicit_target,
            pinned_version,
            &config,
            &permanently_failed,
            config.force,
            finished_roots,
            reporter.as_ref(),
        )
        .map_err(|e| color_eyre::eyre::eyre!(e))
        .and_then(|targets| {
            // The listing is still read, so an unreadable project stops
            // the round here as it always has; the names do not use it.
            ap_build::osc::try_list_project_packages(
                project,
                ap_build::proc::Emit::run(reporter.as_ref()),
            )
            .map_err(|e| color_eyre::eyre::eyre!(e))?;
            Ok(targets)
        });
        let targets = match selected {
            Ok(v) => v,
            Err(e) => {
                // Packages already running are not abandoned: let them
                // finish (their results land in state as usual) first.
                drain_in_flight(&rx, in_flight);
                return Err(e);
            }
        };
        let fresh = unstarted_targets(targets, &in_flight, &done_this_round);
        if fresh.is_empty() && in_flight.is_empty() {
            break;
        }
        // The requested root this invocation already finished is not started
        // again: the closure should have resolved it. Stopping here keeps a
        // resolution defect from rebuilding it round after round.
        let fresh_rpms: Vec<String> = fresh.iter().map(|(_, rpm)| rpm.clone()).collect();
        if let Some(rpm) = finished_reselected(
            &fresh_rpms,
            |rpm| finished_roots.contains(rpm),
            explicit_target.is_some(),
        ) {
            let message = format!(
                "{rpm} already finished built in this run, yet the closure selects it \
                 again; building it again would only repeat that build, so the run stops \
                 here. This is a defect of the dependency resolution to investigate"
            );
            drain_in_flight(&rx, in_flight);
            return Err(color_eyre::eyre::eyre!(message));
        }

        for (t, rpm_name) in &fresh {
            if let Some(tc) = &tui_ctrl {
                tc.note_origin(rpm_name, &t.name);
            }
            reporter.report(&Event::TargetSelected {
                pkg: rpm_name.clone(),
                version: t.version.clone(),
                reason: t.reason.clone(),
                blocked_by: t.blocked_by.clone(),
                from_version: (*rpm_name == config.rpm_name)
                    .then(|| branch.and_then(|b| b.from_version.clone()))
                    .flatten(),
            });
        }

        // One thread per package; OBS builds run concurrently.
        let mut unspawned = fresh.into_iter();
        while let Some((target_info, rpm_name)) = unspawned.next() {
            // `--max-packages`: stop queueing, say so, let the running ones finish.
            let max_packages = run_limits::max_packages();
            if !ap_core::limits::try_start_package(&rpm_name, max_packages) {
                let reason = format!(
                    "not started: --max-packages {max_packages} reached (raise it to build \
                     more packages in one run)"
                );
                for (t, rpm) in std::iter::once((target_info, rpm_name)).chain(unspawned) {
                    reporter.report(&Event::PackageFinished {
                        pkg: rpm,
                        version: t.version.clone(),
                        outcome: PackageOutcome::failed(reason.clone()),
                    });
                }
                drain_in_flight(&rx, in_flight);
                return Err(color_eyre::eyre::eyre!(
                    "--max-packages {max_packages} reached: no more packages are queued in this run"
                ));
            }
            let pkg_config =
                package_config(config, &target_info.name, &target_info.version, &rpm_name);
            let pkg_run = run_dir.pkg(&rpm_name);
            if let Err(e) = pkg_run.ensure() {
                // Every row this round selected must settle: the failing one
                // and the fresh ones never spawned, or the TUI keeps them
                // queued for good. Rows already spawned report themselves.
                let reason = format!(
                    "cannot create the package run directory {}: {e}",
                    pkg_run.path.display()
                );
                reporter.report(&Event::PackageFinished {
                    pkg: rpm_name.clone(),
                    version: target_info.version.clone(),
                    outcome: PackageOutcome::failed(reason.clone()),
                });
                for (rest, rest_rpm) in unspawned {
                    reporter.report(&Event::PackageFinished {
                        pkg: rest_rpm,
                        version: rest.version.clone(),
                        outcome: PackageOutcome::failed(format!(
                            "not started because {rpm_name} has no run directory: {e}"
                        )),
                    });
                }
                // Join the workers already spawned so none is left running
                // unobserved once we return.
                drain_in_flight(&rx, in_flight);
                return Err(color_eyre::eyre::eyre!(reason));
            }
            let r = pkg_reporter(&rpm_name);
            let sem2 = sem.clone();
            let tx = tx.clone();
            let version = target_info.version.clone();
            in_flight.push(rpm_name.clone());
            std::thread::spawn(move || {
                let _permit = Permit::acquire(&sem2);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_package(&pkg_config, &pkg_run, r.as_ref())
                }))
                .unwrap_or_else(|p| PipelineResult::Failed {
                    rpm_name: rpm_name.clone(),
                    reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
                    kind: FailureKind::Other,
                    remedy: None,
                    note: None,
                });
                // Reported as soon as this package is done, not when the
                // scheduler gets to its result, so a slower sibling never
                // holds back the row of one that already finished. The
                // report is itself caught: a panic in a reporter must not
                // skip the send below, or the scheduler waits on a result
                // that never comes (it keeps its own sender alive).
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    r.report(&Event::PackageFinished {
                        pkg: rpm_name.clone(),
                        version: version.clone(),
                        outcome: package_outcome(&result),
                    });
                }));
                let _ = tx.send((rpm_name, version, result));
            });
        }

        // Wait for one package to finish, then select again: its result may
        // have unblocked others.
        let (rpm_name, version, result) = match rx.recv() {
            Ok(r) => r,
            Err(_) => {
                // Every worker is gone without a result: fail the oldest
                // in-flight package rather than panic the scheduler.
                let Some(rpm_name) = in_flight.first().cloned() else {
                    return Err(color_eyre::eyre::eyre!(
                        "package workers ended without reporting"
                    ));
                };
                let result = PipelineResult::Failed {
                    rpm_name: rpm_name.clone(),
                    reason: "build thread ended without reporting a result".to_string(),
                    kind: FailureKind::Other,
                    remedy: None,
                    note: None,
                };
                (rpm_name, String::new(), result)
            }
        };
        in_flight.retain(|r| *r != rpm_name);
        // A result counts as work done in this pass, whoever started it.
        spawned_any = true;
        // The run was aborted (not an ordinary failure): do not count or
        // retry this result, just wait for the rest and stop.
        if ap_build::proc::abort_reason().is_some() {
            drain_in_flight(&rx, in_flight);
            return Err(color_eyre::eyre::eyre!("run aborted"));
        }
        done_this_round.insert(rpm_name.clone());
        finished_roots.note(&config.rpm_name, &rpm_name, &result);

        if !(Frontier {
            fail_counts: fail_counts,
            repeats: repeats,
            rate_limited_counts: rate_limited_counts,
            give_ups: give_ups,
            permanently_failed: permanently_failed,
            attempted_deps: &attempted_deps,
            batch_pending: &mut batch_pending,
            batch_failures: &mut batch_failures,
            patch_requests: &patch_requests,
            native_names: &native_names,
            outdated_names: &outdated_names,
            dep_drops: &dep_drops,
            state_root: &config.state_root,
            pkg_reporter: &pkg_reporter,
            max_target_retries,
        }
        .settle(&rpm_name, &version, result))
        {
            continue;
        }
        // Missing dependencies are handed to the dependency queue now, not
        // when every package still running has finished: a package waiting
        // on `logfire` should not sit behind an hour-long build of another.
        // The packages still running keep running (their results are read
        // when the stream resumes).
        if !batch_pending.is_empty() {
            break;
        }
    }

    Ok(Spawned {
        spawned_any,
        batch_pending,
        batch_failures,
    })
}

/// The config one package of a round is built with. The root package, the
/// one the update command builds, keeps the update and the unknown build
/// system of the config; a dependency of it is an ordinary new build.
pub(super) fn package_config(
    config: &PipelineConfig,
    pkg_name: &str,
    version: &str,
    rpm_name: &str,
) -> PipelineConfig {
    let is_root = rpm_name == config.rpm_name;
    // The root keeps its run as it is; a dependency of it starts from the
    // dependency's config, without the root's update, branch or fix.
    let base = if is_root {
        config.clone()
    } else {
        as_dependency(config)
    };
    PipelineConfig {
        patch_provenance: false,
        carried_recipe: false,
        pkg_name: pkg_name.to_string(),
        rpm_name: rpm_name.to_string(),
        version: version.to_string(),
        // The root package's findings are not a dependency's.
        patch_request: None,
        patch_condition: None,
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root of an update of an unknown build system, as `build_cmd` makes it.
    fn root() -> PipelineConfig {
        PipelineConfig {
            patch_provenance: false,
            carried_recipe: false,
            pkg_name: "cagebreak".into(),
            rpm_name: "cagebreak".into(),
            version: "3.2.1".into(),
            project: "p".into(),
            work_dir: PathBuf::new(),
            state_root: PathBuf::new(),
            rpms_dir: PathBuf::new(),
            distro: "openSUSE_Tumbleweed".into(),
            arch: "x86_64".into(),
            max_attempts: 1,
            budget_usd: 1.0,
            no_agent: true,
            force: false,
            regen: false,
            update: true,
            branch_version: None,
            source_repo: Some("https://github.com/project-repo/cagebreak".into()),
            upstream_tag: None,
            subprojects: Vec::new(),
            failed_deps: Vec::new(),
            unknown_build_system: true,
            unbuildable_deps: Vec::new(),
            request_text: None,
            branch_build: false,
            lint_review: false,
            run_id: "test".into(),
            source_urls: Default::default(),
            python_flavors: Default::default(),
            patch_request: None,
            patch_condition: None,
            bug_fix: None,
        }
    }

    #[test]
    fn the_root_keeps_the_update_and_the_unknown_build_system() {
        let pkg = package_config(&root(), "cagebreak", "3.2.1", "cagebreak");
        assert!(pkg.update);
        assert!(pkg.unknown_build_system);
    }

    #[test]
    fn a_dependency_of_the_root_is_an_ordinary_new_build() {
        let pkg = package_config(&root(), "libfoo", "1.0", "libfoo");
        assert!(!pkg.update);
        assert!(!pkg.unknown_build_system);
        assert!(pkg.branch_version.is_none());
    }

    #[test]
    fn the_fix_task_goes_to_the_root_and_not_to_a_dependency() {
        use ap_build::bugfix::{FixJob, FixTask, FixedIn, Found};
        let task = std::sync::Arc::new(FixTask::Bug(FixJob {
            pkg: "cagebreak".into(),
            refs: Vec::new(),
            description: Some("x".into()),
            bug_texts: Vec::new(),
            read: Vec::new(),
            not_read: Vec::new(),
            warnings: Vec::new(),
            upstream_repo: None,
            patches: Vec::new(),
            candidates: Vec::new(),
            found: Found::NotFound,
            unmerged: Vec::new(),
            fixed_in: FixedIn::NoneYet,
            fetch_hosts: Vec::new(),
            nonce: "n".into(),
        }));
        let config = PipelineConfig {
            patch_provenance: false,
            carried_recipe: false,
            bug_fix: Some(task),
            ..root()
        };
        assert!(
            package_config(&config, "cagebreak", "3.2.1", "cagebreak")
                .bug_fix
                .is_some()
        );
        assert!(
            package_config(&config, "libfoo", "1.0", "libfoo")
                .bug_fix
                .is_none()
        );
    }
}
