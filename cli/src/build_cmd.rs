//! The `build` command: one package's dependency closure, built in parallel.

use crate::*;

mod deps;
mod rows;
mod summary;
mod workers;

pub(crate) use deps::*;
pub(crate) use rows::*;
pub(crate) use summary::*;
pub(crate) use workers::*;
mod spawn;
use spawn::*;
mod ledger;
mod queue;
use ledger::*;
use queue::*;
mod single;
use single::*;
mod frontier;
use frontier::*;
mod resolve;
use resolve::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_build(
    name: &str,
    version: Option<&str>,
    project: &str,
    work_dir: &Path,
    state_root: &Path,
    rpms_dir: &Path,
    distro: &str,
    arch: &str,
    max_attempts: usize,
    budget_usd: f64,
    explicit_target: Option<&str>,
    force: bool,
    regen: bool,
    no_agent: bool,
    sem: Option<Semaphore>,
    // When true, also opportunistically retry every OTHER
    // Failed/Unresolvable/Broken package in the project (not just the ones
    // the requested target's own dep closure needs). Off by default; see
    // the `--sweep` doc on `Commands::Build` for why it is opt-in.
    sweep: bool,
    mut source_repo: Option<String>,
    source_urls: Arc<ap_build::sources::SourceMap>,
    python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    run_dir: &RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
    tui_ctrl: Option<tui::TuiController>,
    // Set by `update`, which builds in a branch project; `None` for
    // every other command.
    branch: Option<&update_project::BranchBuild>,
) -> color_eyre::Result<()> {
    // Reporter for events belonging to a single package: still delivered to
    // jsonl/memory like `reporter`, but tagged so the TUI can attribute
    // per-step events to the right row (see `tui::TuiController::tap`).
    //
    // `PkgContextReporter` sets an ambient pkg name for the duration of each
    // `report()` call so JsonlSink (and any other reporter that cares) can
    // tag its output line with the package name — otherwise JSONL step
    // events from two parallel package threads would be indistinguishable
    // (same `StepId(0)` for both), so the base JSONL stream tags each line
    // with its package as the TUI does via `tap`.
    let pkg_reporter = |pkg: &str| -> Arc<dyn Reporter + Send + Sync> {
        let tagged: Arc<dyn Reporter + Send + Sync> = Arc::new(
            ap_core::sink::PkgContextReporter::new(base_reporter.clone(), Some(pkg.to_string())),
        );
        match &tui_ctrl {
            Some(t) => t.tap(tagged, Some(pkg.to_string())),
            None => tagged,
        }
    };
    // The name this build was requested under, before discovery below may
    // turn it into a repository. A TUI retry resends it (see `tui_ctrl`).
    let request_name = source_repo.clone().unwrap_or_else(|| name.to_string());
    // Resolve the version if it was not pinned. A repository has no package
    // index behind it, so "latest" comes from the forge's own release list.
    let pinned_version = version.map(str::to_string);
    let version = resolve_version(name, version, &mut source_repo, &reporter)?;

    // A repository tells us two things we were only guessing at: whether it is
    // a Python project or a Rust crate, and what it actually calls itself. Ask
    // it before anything is named after the guess.
    // The project's listing is read here so an unreadable one stops the run
    // before anything is named. The name itself is not taken from it: the
    // pipeline resolves the spelling OBS lists the package under (a package
    // OBS has as `python-APScheduler` is checked out under that spelling).
    ap_build::osc::try_list_project_packages(project, ap_build::proc::Emit::run(reporter.as_ref()))
        .map_err(|e| color_eyre::eyre::eyre!(e))?;
    let (name, rpm_name, subprojects) = resolve_names(name, &version, &source_repo, &reporter)?;
    // The run's key is the lowercase guess; the pipeline resolves the spelling
    // OBS lists the package under (`PkgState::obs_name`) in its own preflight.
    // A package branched from another project already has a name, and the
    // pipeline's guess at it (above) is only a guess. Only a repository package
    // is a single named package; a PyPI one is named for each node of its
    // closure, and `update` refuses those whose name would differ.
    let rpm_name = match branch.and_then(|b| b.rpm_name.as_deref()) {
        Some(own) if source_repo.is_some() => own.to_lowercase(),
        _ => rpm_name,
    };
    if let Some(t) = &tui_ctrl {
        t.note_origin(&rpm_name, &request_name);
    }
    let name = name.as_str();
    // The binary names a Python dependency may be published under, for the
    // search `update` does before building one (read once: it runs
    // `rpmspec`).
    let cover_flavors: Vec<String> = if branch.is_some_and(|b| b.resolver.is_some()) {
        python_flavors.effective()
    } else {
        Vec::new()
    };

    let config = PipelineConfig {
        pkg_name: name.to_string(),
        rpm_name: rpm_name.clone(),
        version: version.clone(),
        project: project.to_string(),
        work_dir: work_dir.to_path_buf(),
        state_root: state_root.to_path_buf(),
        rpms_dir: rpms_dir.to_path_buf(),
        distro: distro.to_string(),
        arch: arch.to_string(),
        max_attempts,
        budget_usd,
        no_agent,
        force,
        regen,
        source_repo: source_repo.clone(),
        subprojects,
        run_id: run_dir.run_id.clone().0,
        source_urls,
        python_flavors,
        patch_request: branch.and_then(|b| b.improve.clone()),
        patch_condition: None,
        failed_deps: Vec::new(),
        unbuildable_deps: Vec::new(),
    };

    let mut config = config;

    // Deps discovered at runtime that need to be built before the next resolve.
    let mut pending_deps: Vec<String> = Vec::new();
    // Tracks deps we have already attempted to avoid infinite retry loops.
    let mut attempted_deps: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Dependencies already put behind the packages OBS says they wait on (see
    // the own-project check in the queue below). Each is deferred once, so a
    // dependency whose blockers never get built is still tried afterwards.
    let mut deferred_once: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Dependencies (by `fuzzy_key`) that are being run again on purpose. A
    // local workspace left by their first run would otherwise read as
    // "mid-build through the ordinary frontier" and drop them.
    let mut retrying: std::collections::HashSet<String> = std::collections::HashSet::new();
    // How many rounds each dependency's own build has had without finishing
    // (failed, or came back needing packages of its own), by `fuzzy_key`.
    let mut dep_rounds: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // Rounds a dependency gets before it is written off: `--max-attempts`, like
    // a package named on the command line (see `max_target_retries`), not a
    // smaller number that gives up on it first.
    let max_dep_attempts = max_attempts;
    // Deps seeded below because OBS already reports them Unresolvable, Failed,
    // or Broken. These are by definition already present in the project's own
    // package list, so the "skip deps already present" filter a few lines
    // down — meant for ordinary runtime-discovered deps — must not apply to
    // them, or they can never actually get re-queued. Holds `fuzzy_key`s, not
    // raw names — see where it's populated below.
    let mut stuck_seeds: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Seed the queue with every package OBS itself already reports as
    // Unresolvable, Failed, or Broken in this project — not just ones that
    // happen to be a dependency of whatever target this particular
    // invocation asked for. A previous run can leave a package stuck (its own
    // missing dependency was never itself resolved, the run was interrupted,
    // an agent fix never landed, ...) with nothing left that keeps retrying
    // it once its immediate parent build is done; `--sweep` makes an
    // invocation opportunistically pick those back up too, going through the
    // exact same PyPI-then-source-repo resolution as any other
    // runtime-discovered dependency (see `resolve_dependency_config`). This
    // mirrors the per-target retry `select_target` already does for packages
    // inside the current target's own pip closure (see
    // `pipeline.rs::select_target`'s `resolvable` check) — but covers the
    // whole project, including packages unrelated to whatever target this
    // invocation happens to name.
    //
    // OFF by default: a sweep would broaden a build request beyond its own
    // dependency closure and quietly resolve unrelated stuck packages to
    // current upstream versions. The user opts in with `--sweep`; the default
    // build stays scoped to what the requested target actually needs.
    //
    // `--target` asks for exactly one node and nothing else — see its help
    // text ("Build this specific dependency node instead of the
    // auto-selected one") — so skip the sweep when it's set even if the
    // flag was passed; a `--target` run meant to isolate and debug one
    // package should not pick up unrelated project-wide work.
    seed_stuck_deps(
        project,
        distro,
        arch,
        sweep,
        explicit_target,
        &rpm_name,
        &mut pending_deps,
        &mut stuck_seeds,
    )?;

    // A failed top-level target is retried automatically: it stays in the
    // dependency closure, so the next iteration's `select_target` re-selects
    // it as long as nothing else in the closure needs it built first. Only
    // give up — and stop blocking whatever depends on it — after it has
    // failed this many times in a row. Reuses `--max-attempts` (the same cap
    // on agent fix turns *within* one `run_package` call) so there's a
    // single knob for "how hard to keep trying a broken package", instead of
    // a second, smaller one that gives up before the per-attempt budget does.
    let max_target_retries = max_attempts;
    let mut fail_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // Quota (429) failures say nothing about the package, and with no back-off
    // a package can be rejected many times in quick succession: they get their
    // own, much higher, limit instead of using up the real-failure budget.
    let mut rate_limited_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // Why each package in `permanently_failed` was given up on.
    let mut give_ups: std::collections::BTreeMap<String, String> = Default::default();
    let mut permanently_failed: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Why each dependency left the queue, for the messages that give up.
    let dep_drops = DepDrops::default();
    // Dependencies to branch and patch rather than build.
    let patch_requests = PatchRequests::default();
    let native_names = NativeNames::default();
    let outdated_names = OutdatedNames::default();

    // Packages that are running, and where their results arrive. These outlive
    // one pass of the loop below: a package that reports missing dependencies
    // ends the pass so the dependency queue can build them at once, while the
    // other packages keep running and their results are taken in the next pass.
    let (tx, rx) = std::sync::mpsc::channel::<(String, String, PipelineResult)>();
    let mut in_flight: Vec<String> = Vec::new();

    // Resolve closure, select all buildable targets, build them in parallel,
    // then repeat until the entire closure is built (or an error occurs).
    loop {
        // Before re-resolving, build any runtime-discovered deps (e.g. test-only
        // deps the agent found to be unresolvable).
        // Groups of dependencies set aside until what they wait on has had its
        // batch (see `retry_group` below). The most recent group runs first
        // once nothing else is pending: it waits on the newest packages, and a
        // package queued behind it may wait on it in turn.
        drain_dep_queue(Build {
            project,
            distro,
            arch,
            rpm_name: &rpm_name,
            version: &version,
            cover_flavors: &cover_flavors,
            branch,
            state_root,
            reporter: &reporter,
            pkg_reporter: &pkg_reporter,
            tui_ctrl: &tui_ctrl,
            sem: &sem,
            run_dir,
            dep_drops: &dep_drops,
            patch_requests: &patch_requests,
            native_names: &native_names,
            outdated_names: &outdated_names,
            attempted_deps: &mut attempted_deps,
            deferred_once: &mut deferred_once,
            retrying: &mut retrying,
            dep_rounds: &mut dep_rounds,
            stuck_seeds: &mut stuck_seeds,
            pending_deps: &mut pending_deps,
            config: &mut config,
            max_dep_attempts,
        })?;

        if source_repo.is_some() {
            match single_repo_build(Build {
                project,
                distro,
                arch,
                rpm_name: &rpm_name,
                version: &version,
                cover_flavors: &cover_flavors,
                branch,
                state_root,
                reporter: &reporter,
                pkg_reporter: &pkg_reporter,
                tui_ctrl: &tui_ctrl,
                sem: &sem,
                run_dir,
                dep_drops: &dep_drops,
                patch_requests: &patch_requests,
                native_names: &native_names,
                outdated_names: &outdated_names,
                attempted_deps: &mut attempted_deps,
                deferred_once: &mut deferred_once,
                retrying: &mut retrying,
                dep_rounds: &mut dep_rounds,
                stuck_seeds: &mut stuck_seeds,
                pending_deps: &mut pending_deps,
                config: &mut config,
                max_dep_attempts,
            })? {
                Flow::Done => return Ok(()),
                Flow::Retry => continue,
            }
        }

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
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!("interrupted"));
            }
            // The package names OBS lists are what builds and tracking use, so
            // the listing is read with the frontier: a failure of either ends
            // the build, after the packages already running have finished.
            let selected = select_target(
                name,
                explicit_target,
                pinned_version.as_deref(),
                &config,
                &permanently_failed,
                config.force,
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
                    drain_in_flight(&rx, &mut in_flight);
                    return Err(e);
                }
            };
            let fresh = unstarted_targets(targets, &in_flight, &done_this_round);
            if fresh.is_empty() && in_flight.is_empty() {
                break;
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
                            outcome: PackageOutcome::Failed {
                                reason: reason.clone(),
                                disk_full: false,
                            },
                        });
                    }
                    drain_in_flight(&rx, &mut in_flight);
                    return Err(color_eyre::eyre::eyre!(
                        "--max-packages {max_packages} reached: no more packages are queued in this run"
                    ));
                }
                let pkg_config = PipelineConfig {
                    pkg_name: target_info.name.clone(),
                    rpm_name: rpm_name.clone(),
                    version: target_info.version.clone(),
                    // The root package's findings are not a dependency's.
                    patch_request: None,
                    patch_condition: None,
                    ..config.clone()
                };
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
                        outcome: PackageOutcome::Failed {
                            reason: reason.clone(),
                            disk_full: false,
                        },
                    });
                    for (rest, rest_rpm) in unspawned {
                        reporter.report(&Event::PackageFinished {
                            pkg: rest_rpm,
                            version: rest.version.clone(),
                            outcome: PackageOutcome::Failed {
                                reason: format!(
                                    "not started because {rpm_name} has no run directory: {e}"
                                ),
                                disk_full: false,
                            },
                        });
                    }
                    // Join the workers already spawned so none is left running
                    // unobserved once we return.
                    drain_in_flight(&rx, &mut in_flight);
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
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!("run aborted"));
            }
            done_this_round.insert(rpm_name.clone());

            if !(Frontier {
                fail_counts: &mut fail_counts,
                rate_limited_counts: &mut rate_limited_counts,
                give_ups: &mut give_ups,
                permanently_failed: &mut permanently_failed,
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

        // Nothing was started this round: the closure is either built or
        // blocked, which is the terminal state checked here.
        if !spawned_any {
            if permanently_failed.is_empty() {
                ap_core::diag!("{name} and all dependencies are already built");
                return Ok(());
            }
            for pkg in &permanently_failed {
                give_ups
                    .entry(pkg.clone())
                    .or_insert_with(|| "no reason was recorded".to_string());
            }
            return Err(color_eyre::eyre::eyre!(build_incomplete_message(&give_ups)));
        }

        // Compute what, if anything, from `batch_pending` is actually new
        // information *before* merging it into `pending_deps` — the
        // explicit-target branch below needs to decide "did this round
        // discover any new missing deps, or are we spinning on the same ones
        // every pass" and the merged list hides that.
        let new_pending: Vec<String> = batch_pending
            .iter()
            .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
            .cloned()
            .collect();
        pending_deps.extend(batch_pending);

        // If an explicit target was requested, stop after one batch — there is
        // no "next round" to retry it in, so a failure here must surface as
        // an error rather than a silent `Ok`.
        if explicit_target.is_some() {
            if let Some((rpm_name, reason)) = batch_failures.first() {
                // Packages the batch left running are joined before returning.
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
            }
            // A `--target` run returning `MissingDeps` must not report success
            // for a target that was *not* built. Make unresolved work a
            // non-success outcome: if
            // this batch surfaced genuinely new deps (not ones we have
            // already tried and failed/gave-up on), loop around once to let
            // them build via the top-of-loop pending-deps processor and
            // re-select the target; if we would loop on the same deps
            // forever, error out instead.
            if !pending_deps.is_empty() && new_pending.is_empty() {
                let missing: Vec<&String> = pending_deps.iter().collect();
                let names = missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!(
                    "{name} needs {} but every one has already been attempted \
                     without success; cannot build. Why each one is out of reach:\n{}",
                    names,
                    dep_drops.explain(&pending_deps)
                ));
            }
            if pending_deps.is_empty() {
                return Ok(());
            }
            // Fall through to the next loop iteration so `pending_deps` drains
            // (building each new dep), then `select_target` re-selects the
            // same explicit target and `run_package` retries with those deps
            // now available.
        }
    }
}
