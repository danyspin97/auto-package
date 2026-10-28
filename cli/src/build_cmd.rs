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
    const MAX_RATE_LIMITED_ROUNDS: usize = 200;
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
        let mut retry_stack: Vec<Vec<String>> = Vec::new();
        while !pending_deps.is_empty() || !retry_stack.is_empty() {
            if pending_deps.is_empty() {
                pending_deps = retry_stack.pop().unwrap_or_default();
            }
            let batch = std::mem::take(&mut pending_deps);
            let mut newly_unbuildable: Vec<String> = Vec::new();
            // (dependency, the packages it waits on) for each dependency OBS
            // reports unresolvable in this repository/architecture.
            let mut deferred: Vec<(String, Vec<String>)> = Vec::new();
            // Everything the availability filter below needs is read once for
            // the batch, up front. A listing that cannot be read stops the
            // run: treated as empty, it would queue packages OBS already has.
            let lookup_emit = ap_build::proc::Emit::run(reporter.as_ref());
            let own_listing = ap_build::osc::try_list_project_packages(project, lookup_emit)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            let own_statuses = ap_build::osc::get_project_build_statuses(project, distro, arch)?;
            let mut linked_listings = Vec::new();
            for (path_project, _repo) in
                ap_build::osc::get_project_repo_paths(project, lookup_emit)?
            {
                let listing = ap_build::osc::try_list_project_packages(&path_project, lookup_emit)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                linked_listings.push((path_project, listing));
            }
            // What a dependency build runs as: its own thread, reported as soon
            // as it is done. Reporting in join order let a slower sibling hold
            // back every package finished after it, so their rows stayed on
            // their last step.
            let spawn_dep_build = |dep_config: PipelineConfig, dep: String| {
                let dep_rpm = dep_config.rpm_name.clone();
                let dep_pkg_run = run_dir.pkg(&dep_rpm);
                if let Err(e) = dep_pkg_run.ensure() {
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!(
                            "its run directory {} could not be created: {e}",
                            dep_pkg_run.path.display()
                        ),
                    );
                    return None;
                }
                reporter.report(&Event::TargetSelected {
                    pkg: dep_rpm.clone(),
                    version: dep_config.version.clone(),
                    reason: SelectionReason::FirstUnbuiltDependency,
                    blocked_by: vec![],
                    from_version: None,
                });
                // A failed request under the dependency's name is settled by this
                // row (see `withdraw_settled_failures`). It is not a request, so
                // a retry of this row must not resend the name (see `request_names`).
                if let Some(t) = &tui_ctrl {
                    t.note_label(&dep_rpm, &dep);
                }
                let dep_version = dep_config.version.clone();
                let r = pkg_reporter(&dep_rpm);
                let sem2 = sem.clone();
                let finished_name = dep_rpm.clone();
                let finished_version = dep_version.clone();
                let handle = std::thread::spawn(move || {
                    let _permit = Permit::acquire(&sem2);
                    // A panic is a failure of this row, reported like any other
                    // outcome, so the row settles and the run sees it finish.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_package(&dep_config, &dep_pkg_run, r.as_ref())
                    }))
                    .unwrap_or_else(|p| PipelineResult::Failed {
                        rpm_name: finished_name.clone(),
                        reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
                        kind: FailureKind::Other,
                    });
                    r.report(&Event::PackageFinished {
                        pkg: finished_name,
                        version: finished_version,
                        outcome: package_outcome(&result),
                    });
                    result
                });
                Some((dep_rpm, dep_version, handle, dep))
            };
            // A dependency a package asked to have patched is branched from the
            // linked project that ships it, not looked up on PyPI or a forge.
            let (patch_batch, batch): (Vec<String>, Vec<String>) = batch
                .into_iter()
                .partition(|d| patch_requests.get(d).is_some());
            let mut patch_handles = Vec::new();
            for rpm_dep in patch_batch {
                let dep = ap_build::osc::strip_rpm_prefix(&rpm_dep).to_string();
                let Some(request) = patch_requests.get(&dep) else {
                    continue;
                };
                if !attempted_deps.insert(ap_build::osc::fuzzy_key(&dep)) {
                    let earlier = dep_drops
                        .reason(&dep)
                        .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!("it was already attempted this run ({earlier})"),
                    );
                    continue;
                }
                match branch_for_patch(
                    &dep,
                    &request,
                    project,
                    &config,
                    &own_listing,
                    &linked_listings,
                    lookup_emit,
                ) {
                    Ok(dep_config) => {
                        if let Some(h) = spawn_dep_build(dep_config, dep.clone()) {
                            patch_handles.push(h);
                        }
                    }
                    Err(why) => {
                        dep_drops.dropped(reporter.as_ref(), &dep, why);
                    }
                }
            }
            // A dependency the repository has at an older version than a
            // dependent needs is updated, not dropped as available.
            let (outdated_batch, batch): (Vec<String>, Vec<String>) =
                batch.into_iter().partition(|d| outdated_names.contains(d));
            for rpm_dep in outdated_batch {
                let dep = ap_build::osc::strip_rpm_prefix(&rpm_dep).to_string();
                if !attempted_deps.insert(ap_build::osc::fuzzy_key(&dep)) {
                    let earlier = dep_drops
                        .reason(&dep)
                        .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!("it was already attempted this run ({earlier})"),
                    );
                    continue;
                }
                match update_config_for_outdated(
                    &dep,
                    outdated_names.required(&dep).as_deref(),
                    project,
                    &config,
                    &own_listing,
                    reporter.as_ref(),
                    lookup_emit,
                ) {
                    Ok(dep_config) => {
                        if let Some(h) = spawn_dep_build(dep_config, dep.clone()) {
                            patch_handles.push(h);
                        }
                    }
                    Err(why) => dep_drops.dropped(reporter.as_ref(), &dep, why),
                }
            }
            // Every reported name is triaged once (see `depclass::triage`):
            // the `python<ver>-` prefix and `-devel` wrapping come off, flavors
            // and repeats of one package collapse, and what is not something to
            // build (a capability, an interpreter package, a vendor library, a
            // system tool, something the distro provides) is dropped with its
            // reason. Each of those is final for the run, so it counts as
            // attempted: a package that keeps re-reporting the name is then
            // recognized as making no progress (see `new_deps` below).
            let mut in_batch = std::collections::HashSet::new();
            let mut triaged: Vec<String> = Vec::new();
            // Names `triage` found to be native libraries, by `fuzzy_key`.
            let mut native_keys: std::collections::HashSet<String> = Default::default();
            for raw in batch {
                match ap_build::depclass::triage(&raw, &|c| {
                    ap_build::osc::available_to_project(project, arch, c, lookup_emit)
                }) {
                    ap_build::depclass::Triage::Build { name, kind } => {
                        if kind == ap_build::depclass::PackageKind::Native
                            || native_names.contains(&name)
                        {
                            native_keys.insert(ap_build::osc::fuzzy_key(&name));
                        }
                        if in_batch.insert(ap_build::osc::fuzzy_key(&name)) {
                            triaged.push(name);
                        }
                    }
                    ap_build::depclass::Triage::Skip(ap_build::depclass::Skip::Empty) => {}
                    ap_build::depclass::Triage::Skip(skip) => {
                        attempted_deps.insert(ap_build::osc::fuzzy_key(&raw));
                        dep_drops.dropped(reporter.as_ref(), &raw, skip.reason());
                        if skip.is_unbuildable() && !newly_unbuildable.contains(&raw) {
                            newly_unbuildable.push(raw);
                        }
                    }
                }
            }
            // A native library the projects do not publish under this name but
            // do publish under similar ones (`ffmpeg` against `ffmpeg-8`,
            // `ffmpeg-8-mini-devel`) is the distro's own package: building it
            // from source here would be a second, worse copy.
            triaged.retain(|dep| {
                if !native_keys.contains(&ap_build::osc::fuzzy_key(dep)) {
                    return true;
                }
                if ap_build::osc::project_ships_binary(project, arch, dep, lookup_emit)
                    != Some(false)
                {
                    return true;
                }
                let similar =
                    ap_build::osc::similar_published_binaries(project, arch, dep, lookup_emit);
                if similar.is_empty() {
                    return true;
                }
                attempted_deps.insert(ap_build::osc::fuzzy_key(dep));
                dep_drops.dropped(
                    reporter.as_ref(),
                    dep,
                    format!(
                        "the distro ships it under other names ({}), which a package \
                         requires instead of building a copy from source",
                        similar.join(", ")
                    ),
                );
                false
            });
            // Dependencies dropped only because a build of them is already
            // underway: a later request for one must not be refused as "already
            // attempted" if that build then fails.
            let mid_build: std::cell::RefCell<Vec<String>> = Default::default();
            let handles: Vec<_> = triaged
                .into_iter()
                .filter(|dep| {
                    // A dependency we already built here is available, even while
                    // OBS has not indexed it yet. Its version is logged so a bound
                    // it does not meet is still visible.
                    match built_locally(state_root, &ap_build::osc::rpm_name(dep)) {
                        Some(version) => {
                            dep_drops.dropped(
                                reporter.as_ref(),
                                dep,
                                format!("it was already built locally at {version}"),
                            );
                            false
                        }
                        None => true,
                    }
                })
                .filter(|dep| {
                    // Counted as attempted whatever a later filter does with
                    // it, so a package that keeps re-reporting a name that is
                    // already satisfied (in the project, a linked project)
                    // is still recognized as making no progress instead of
                    // retrying forever.
                    let fresh = attempted_deps.insert(ap_build::osc::fuzzy_key(dep));
                    if !fresh {
                        let earlier = dep_drops
                            .reason(dep)
                            .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!("it was already attempted this run ({earlier})"),
                        );
                    }
                    fresh
                })
                .filter(|dep| {
                    // Skip deps already resolvable in the project or any of
                    // its configured build/path projects — unless the
                    // package is in OUR project but in a non-resolvable
                    // state (Failed/Unresolvable/Broken/Disabled/Excluded/
                    // Unknown/missing-status), which is exactly the case
                    // this queue exists to retry.
                    //
                    // A name present in the union of listed packages is not
                    // enough: a source entry in the own project with status
                    // Disabled, or no entry at all for the requested
                    // distro/arch, must not count as "present → resolved",
                    // or the dep is dropped from the queue though no built
                    // binary exists. The UnresolvableDeps path from
                    // `pipeline.rs` reaches here, so dropping a dep here
                    // would make that whole queue appear to do nothing.
                    //
                    // Linked (upstream) project packages are still trusted
                    // on mere presence — we don't own or rebuild those and
                    // their per-arch build status is unreliable to query
                    // from a project we don't control (see
                    // `select_target`'s `linked_packages` comment).
                    //
                    // The ecosystem (and so the real RPM name) is not
                    // known yet at this point — a system package like
                    // `cmake` or `cuda-devel` never gets the `python-`
                    // prefix a Python dependency would, so check both
                    // spellings rather than assuming Python. Everything
                    // here compares by `fuzzy_key`, not the raw string.
                    let bare_key = ap_build::osc::fuzzy_key(dep);
                    if stuck_seeds.contains(&bare_key) {
                        return true;
                    }
                    // Own-project packages are matched by fuzzy key so a
                    // separator mismatch can't false-miss; of several
                    // spellings the built one counts (see `availability`).
                    let found = ap_build::depclass::availability(
                        dep,
                        &own_listing,
                        &own_statuses,
                        &linked_listings,
                    );
                    if let ap_build::depclass::Availability::InProject {
                        pkg: real_pkg,
                        status,
                    } = &found
                    {
                        let real_pkg = real_pkg.as_str();
                        let status = status.as_ref();
                        // Package exists in our project. Only skip queuing
                        // it when its OBS build status is actually
                        // resolvable — i.e. there's a built binary (or one
                        // genuinely in flight). An Unresolvable/Failed/
                        // Broken/Disabled/Excluded/Unknown or missing-
                        // status entry stays queued so the agent can look
                        // at it.
                        // The status is the one for the repository and
                        // architecture being built (`own_statuses` was read
                        // for exactly that pair): the same package being
                        // excluded or unresolvable on another one says
                        // nothing about this build.
                        let skip = ap_build::pipeline::status_is_resolvable(status);
                        if matches!(status, Some(ap_build::osc::BuildStatus::Excluded)) {
                            // No rebuild changes a project/spec exclusion, and
                            // whatever needs this package cannot be built
                            // here either; say so instead of running a build
                            // that can only report it.
                            dep_drops.dropped(
                                reporter.as_ref(),
                                dep,
                                format!(
                                    "{real_pkg} is excluded for {distro}/{arch} in {project} \
                                     (ExclusiveArch/ExcludeArch in its spec, or the project \
                                     config), so nothing that needs it can be built for \
                                     {arch}"
                                ),
                            );
                            return false;
                        }
                        if matches!(status, Some(ap_build::osc::BuildStatus::Unresolvable))
                            && !deferred_once.contains(&bare_key)
                        {
                            // The package exists but cannot build because
                            // packages it needs are missing; OBS names them.
                            // Build those first, then come back to it.
                            let blockers: Vec<String> = ap_build::osc::unresolvable_blockers(
                                project, real_pkg, distro, arch,
                            )
                            .into_iter()
                            .filter(|b| {
                                ap_build::osc::built_in_project(&own_listing, &own_statuses, b)
                                    .is_none()
                            })
                            .collect();
                            if !blockers.is_empty() {
                                deferred_once.insert(bare_key.clone());
                                reporter.report(&Event::Progress {
                                    step: None,
                                    message: format!(
                                        "{dep} exists in {project} as {real_pkg} but is \
                                         unresolvable for {distro}/{arch}: building what it \
                                         waits on first ({})",
                                        blockers.join(", ")
                                    ),
                                });
                                deferred.push((dep.clone(), blockers));
                                return false;
                            }
                        }
                        if skip {
                            let mut reason = format!(
                                "it is already in {project} as {real_pkg} with status \
                                 {:?}, which counts as resolvable",
                                status.map(|s| s.to_string()).unwrap_or_default()
                            );
                            // Built is not the same as usable: a skeleton spec
                            // "succeeds" with an empty RPM.
                            if matches!(status, Some(ap_build::osc::BuildStatus::Succeeded))
                                && let Some(unusable) = ap_build::osc::empty_package_warning(
                                    project, real_pkg, distro, arch,
                                )
                            {
                                reason = format!("{reason}, but {unusable}");
                            }
                            dep_drops.dropped(reporter.as_ref(), dep, reason);
                        } else {
                            ap_core::diag!(
                                "dep queue: keeping {dep} (in {project} as \
                                 {real_pkg} with status {:?} — not \
                                 resolvable, retrying)",
                                status.map(|s| s.to_string()).unwrap_or_default()
                            );
                        }
                        return !skip;
                    }

                    // Linked / build-path projects: trust presence (see
                    // comment above). That only holds for a requirement
                    // without a version bound: when the package that needs it
                    // still reports it unresolvable afterwards, the shipped
                    // version is too old, which the give-up message says.
                    if let ap_build::depclass::Availability::InLinked {
                        project: path_project,
                        pkg: match_name,
                    } = &found
                    {
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!(
                                "it is available from the linked project {path_project} as \
                                 {match_name} (if the package needing it still reports it \
                                 unresolvable, that version does not meet its bound: it needs \
                                 a newer release built in {project}, or a patch_dependencies \
                                 request)"
                            ),
                        );
                        return false;
                    }
                    // A local workspace already existing for this name means
                    // some earlier round already checked it out — most
                    // likely it's a node in the target's own pip-resolution
                    // closure, mid-build or queued for the next round via
                    // the ordinary frontier mechanism (`select_target`),
                    // which knows nothing about this ad-hoc queue and will
                    // keep retrying it on its own. A second, redundant
                    // report of the same dependency here (a real case: one
                    // package's agent reporting `z3-solver` as missing while
                    // `python-z3-solver` is simultaneously mid-build
                    // elsewhere in the very same run — OBS genuinely didn't
                    // have it *yet*, but "not yet" isn't "nobody's on it")
                    // must not spawn a second, parallel build attempt. This
                    // check comes after the `stuck_seeds` early-return above,
                    // not before: a genuinely stuck package also has local
                    // state, and that's exactly the case this queue exists
                    // to retry.
                    let state_path = config
                        .state_root
                        .join(ap_core::run::sanitize_name(&ap_build::osc::rpm_name(dep)))
                        .join("state.json");
                    let keep = !state_path.exists() || retrying.contains(&bare_key);
                    if !keep {
                        mid_build.borrow_mut().push(bare_key.clone());
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!(
                                "a local workspace at {} already exists, so it is \
                                 probably mid-build through the ordinary frontier, which \
                                 keeps retrying it on its own",
                                state_path.display()
                            ),
                        );
                    }
                    keep
                })
                .filter(|dep| {
                    // `update`: a dependency another project already
                    // publishes becomes a build path of the branch project, or
                    // is already reachable, instead of a second copy built here.
                    let Some(resolver) = branch.and_then(|b| b.resolver.as_ref()) else {
                        return true;
                    };
                    let mut candidates = vec![dep.clone(), ap_build::osc::rpm_name(dep)];
                    candidates.extend(cover_flavors.iter().map(|f| format!("{f}-{dep}")));
                    match resolver.cover_dependency(&candidates) {
                        Ok(Some(note)) => {
                            dep_drops.dropped(reporter.as_ref(), dep, note);
                            false
                        }
                        Ok(None) => true,
                        Err(e) => {
                            reporter.report(&Event::Warning {
                                step: None,
                                message: format!(
                                    "could not search OBS for {dep}: {e}; building it instead"
                                ),
                            });
                            true
                        }
                    }
                })
                .filter_map(|dep| {
                    let native = native_keys.contains(&ap_build::osc::fuzzy_key(&dep));
                    let dep_config =
                        match resolve_dependency_config(&dep, native, &config, reporter.as_ref()) {
                            Some(c) => c,
                            None => {
                                // No PyPI project and no discoverable source repo —
                                // this is never going to become buildable no matter
                                // how many times it's reported. Record it so every
                                // package needing it learns that from its next
                                // prompt (convention 28 Exception 4) instead of
                                // reporting it again forever.
                                dep_drops.dropped(
                                    reporter.as_ref(),
                                    &dep,
                                    "it is unresolvable: no PyPI release and no usable public \
                                     source repository (the warning above lists what was tried)",
                                );
                                newly_unbuildable.push(dep);
                                return None;
                            }
                        };
                    spawn_dep_build(dep_config, dep)
                })
                .collect();
            let handles: Vec<_> = handles.into_iter().chain(patch_handles).collect();

            // A request that was only set aside for a build already underway
            // may be made again if that build fails, a bounded number of times.
            for key in mid_build.into_inner() {
                let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                *rounds += 1;
                if *rounds < max_dep_attempts {
                    attempted_deps.remove(&key);
                }
            }

            for dep in newly_unbuildable {
                if !config.unbuildable_deps.contains(&dep) {
                    config.unbuildable_deps.push(dep);
                }
            }

            // Dependencies to run again once what they wait on has had its
            // batch: those OBS reports unresolvable, and (below) those that
            // came back needing packages of their own.
            let mut retry_group: Vec<String> = Vec::new();
            for (dep, blockers) in deferred {
                attempted_deps.remove(&ap_build::osc::fuzzy_key(&dep));
                retrying.insert(ap_build::osc::fuzzy_key(&dep));
                pending_deps.extend(blockers);
                retry_group.push(dep);
            }

            for (dep_rpm, _version, handle, dep) in handles {
                let result = join_package(handle, &dep_rpm);
                // Aborted run: the results are not outcomes of the packages.
                if ap_build::proc::abort_reason().is_some() {
                    continue;
                }
                let key = ap_build::osc::fuzzy_key(&dep);
                match result {
                    PipelineResult::Built { .. } => {
                        ap_core::diag!("built dep {dep_rpm}");
                        dep_drops.note_outcome(
                            &dep_rpm,
                            "it was built this run, yet the package that needs it still \
                             reports it missing (a name mismatch?)",
                        );
                    }
                    PipelineResult::AlreadyBuilt { .. } => {
                        ap_core::diag!("dep {dep_rpm} already built");
                        dep_drops.note_outcome(
                            &dep_rpm,
                            "it was already built, yet the package that needs it still \
                             reports it missing (a name mismatch?)",
                        );
                    }
                    PipelineResult::MissingDeps {
                        deps: more,
                        patches,
                        native,
                        outdated,
                        ..
                    } => {
                        patch_requests.record(&patches);
                        native_names.record(&native);
                        outdated_names.record(&outdated);
                        dep_drops.note_outcome(
                            &dep_rpm,
                            format!("it could not be built until {} exist", more.join(", ")),
                        );
                        // It needs packages of its own: once those have had
                        // their batch it is run again, instead of staying
                        // "attempted" while the package waiting on it is
                        // abandoned. Bounded like any other retry.
                        let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                        *rounds += 1;
                        if !more.is_empty() && *rounds < max_dep_attempts {
                            attempted_deps.remove(&key);
                            retrying.insert(key);
                            retry_group.push(dep);
                        }
                        pending_deps.extend(more);
                    }
                    PipelineResult::Failed { reason, kind, .. } => {
                        // A dependency OBS or the pipeline failed is run
                        // again through the normal pipeline when the package
                        // needing it names it once more, rather than counted
                        // as attempted and dropped after one go — unless it
                        // used up its allowance, which a second round would
                        // only repeat. Rounds are bounded and an attempt
                        // within a round resumes the package's persisted
                        // attempt count.
                        let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                        *rounds += 1;
                        let retry = dep_failure_retries(kind, *rounds, max_dep_attempts);
                        if retry {
                            attempted_deps.remove(&key);
                            retrying.insert(key);
                            // Run again once the rest of this batch has had its
                            // turn, whether or not a dependent names it again:
                            // the dependent may have given up on seeing it failed.
                            retry_group.push(dep.clone());
                        }
                        dep_drops.note_outcome(
                            &dep_rpm,
                            format!(
                                "its build failed ({reason}){}",
                                if retry {
                                    "; it is tried once more if it is still needed"
                                } else {
                                    ""
                                }
                            ),
                        );
                        pkg_reporter(&dep_rpm).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "dependency {dep_rpm} failed to build ({reason}); \
                                 retrying the package that needs it"
                            ),
                        });
                        // Record it so the dependent package's agent can see
                        // that this one was attempted — without that evidence
                        // it can only report the dep again and stall.
                        if !config.failed_deps.contains(&dep_rpm) {
                            config.failed_deps.push(dep_rpm);
                        }
                    }
                }
            }
            if ap_build::proc::abort_reason().is_some() {
                return Err(color_eyre::eyre::eyre!("run aborted"));
            }
            if !retry_group.is_empty() {
                retry_stack.push(retry_group);
            }
        }

        // A repository target is a single package: there is no PyPI closure to
        // walk, and pip could not resolve the name even if we asked it to.
        // Missing deps surface as MissingDeps; we build them via the PyPI path
        // in the pending_deps loop above, then retry this package.
        if source_repo.is_some() {
            let pkg_run = run_dir.pkg(&rpm_name);
            pkg_run.ensure()?;
            reporter.report(&Event::TargetSelected {
                pkg: rpm_name.clone(),
                version: version.clone(),
                reason: SelectionReason::ExplicitOverride,
                blocked_by: vec![],
                from_version: branch.and_then(|b| b.from_version.clone()),
            });
            let r = pkg_reporter(&rpm_name);
            let result = run_package(&config, &pkg_run, r.as_ref());
            let outcome = package_outcome(&result);
            r.report(&Event::PackageFinished {
                pkg: rpm_name.clone(),
                version: version.clone(),
                outcome,
            });
            match result {
                PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => return Ok(()),
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
                    continue;
                }
                PipelineResult::Failed { reason, .. } => {
                    return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
                }
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

            match result {
                PipelineResult::Built { .. } => {
                    ap_core::diag!("built {rpm_name} {version}");
                    fail_counts.remove(&rpm_name);
                }
                PipelineResult::AlreadyBuilt { .. } => {
                    ap_core::diag!("{rpm_name} {version} already built");
                    fail_counts.remove(&rpm_name);
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
                    // No-progress detection. A package that keeps
                    // surfacing MissingDeps on names already in
                    // `attempted_deps` — and therefore skipped by the
                    // mini-loop every time — is not making progress; it is
                    // blocked on something that can never be built in this
                    // environment (closed-source toolchain, PyPI name that
                    // doesn't exist, …). Without this, the frontier would
                    // re-select the same package forever: MissingDeps does
                    // not consume the outer failed-target limit, so the
                    // loop never gives up on it.
                    //
                    // Treat such a no-progress round the same as a Failed
                    // result for retry-counting purposes: increment
                    // `fail_counts`, and after `max_target_retries` of
                    // genuinely-no-new-deps rounds, mark the package
                    // permanently_failed and let the frontier move on.
                    //
                    // A dep attempted earlier may have been built locally since
                    // (by a sibling in this round, say); it is available now, so
                    // it counts as progress, not as an exhausted dependency.
                    let remaining: Vec<&String> = deps
                        .iter()
                        .filter(|d| {
                            built_locally(&config.state_root, &ap_build::osc::rpm_name(d)).is_none()
                        })
                        .collect();
                    let new_deps: Vec<&&String> = remaining
                        .iter()
                        .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
                        .collect();
                    if new_deps.is_empty() && !remaining.is_empty() {
                        // Repeating this package cannot help: every dependency it
                        // names was already attempted and dropped by the dep queue,
                        // and attempted names are never queued again. Give up on the
                        // first such round instead of re-running the pipeline up to
                        // `max_target_retries` times at real agent cost.
                        let count = fail_counts.entry(rpm_name.clone()).or_insert(0);
                        *count += 1;
                        permanently_failed.insert(rpm_name.clone());
                        give_ups.insert(
                            rpm_name.clone(),
                            format!(
                                "it needs {} and none of them can be queued again:\n{}",
                                deps.join(", "),
                                dep_drops.explain(&deps)
                            ),
                        );
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "giving up on {rpm_name}: it needs {} and each of those was \
                                 already attempted and dropped; anything depending on it \
                                 stays blocked. Why each one is out of reach:\n{}",
                                deps.join(", "),
                                dep_drops.explain(&deps)
                            ),
                        });
                    }
                    batch_pending.extend(deps);
                }
                PipelineResult::Failed { reason, kind, .. } => {
                    batch_failures.push((rpm_name.clone(), reason.clone()));
                    if kind == FailureKind::Busy {
                        // Another process is building this package, so nothing
                        // was tried here. It is deferred to a later round and
                        // does not count toward giving up.
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "{rpm_name} is being built by another process ({reason}); \
                                 deferred to a later round"
                            ),
                        });
                        continue;
                    }
                    let rate_limited = kind == FailureKind::RateLimited;
                    // A package that already spent its *entire* budget or attempt
                    // allowance on this round gives up at once, instead of
                    // getting a fresh budget for each of `max_target_retries`
                    // rounds. `max_target_retries` reuses `max_attempts` as the
                    // whole-round retry count, so without this a package that
                    // burned its budget would repeat the same loop over and over.
                    let exhausted = kind == FailureKind::Exhausted;
                    // A package whose resolution ladder was spent is not run
                    // again: the same spec would fail the same ladder again. It
                    // is a failure of the tool to investigate, never a `blocked`.
                    let ladder = reason.starts_with(ap_build::pipeline::LADDER_FAILED_PREFIX);
                    let count = if rate_limited {
                        rate_limited_counts.entry(rpm_name.clone()).or_insert(0)
                    } else {
                        fail_counts.entry(rpm_name.clone()).or_insert(0)
                    };
                    *count += 1;
                    let limit = if rate_limited {
                        MAX_RATE_LIMITED_ROUNDS
                    } else {
                        max_target_retries
                    };
                    if exhausted || *count >= limit {
                        permanently_failed.insert(rpm_name.clone());
                        give_ups.insert(
                            rpm_name.clone(),
                            if ladder {
                                format!("every resolution rung failed ({reason})")
                            } else if exhausted {
                                format!("it spent its full budget/attempt allowance ({reason})")
                            } else {
                                format!("it failed {count} times in a row (last: {reason})")
                            },
                        );
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: if ladder {
                                format!(
                                    "{rpm_name} is unresolved: every rung of the resolution \
                                     ladder failed ({reason}); this is a defect of the tool to \
                                     investigate. Anything depending on it stays waiting, the \
                                     rest of the closure keeps going"
                                )
                            } else if exhausted {
                                format!(
                                    "giving up on {rpm_name}: it already spent its full \
                                     budget/attempt allowance this round without succeeding \
                                     ({reason}) — another round would just repeat the same \
                                     fix loop with a fresh budget; anything depending on it \
                                     stays blocked, but the rest of the closure keeps going"
                                )
                            } else {
                                format!(
                                    "giving up on {rpm_name} after {count} failed attempts \
                                     ({reason}); anything depending on it stays blocked, but \
                                     the rest of the closure keeps going"
                                )
                            },
                        });
                    } else {
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "{rpm_name} failed (attempt {count}/{limit}): \
                                 {reason}; retrying next round"
                            ),
                        });
                    }
                }
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

/// The version to build: the pinned one, or the latest release of the
/// source repository, the name on the command line, or the repository
/// discovered for it. `source_repo` is set when a repository is discovered.
fn resolve_version(
    name: &str,
    version: Option<&str>,
    source_repo: &mut Option<String>,
    reporter: &Arc<dyn Reporter + Send + Sync>,
) -> color_eyre::Result<String> {
    Ok(match version {
        Some(v) => v.to_string(),
        None => {
            if let Some(repo) = &source_repo {
                let v = ap_build::sources::latest_release(repo)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!("{repo} latest release is {v}"),
                });
                v
            } else if let Some(v) =
                ap_build::redist::latest_version(&ap_build::redist::Network, name)
            {
                // `--nvidia-redist`: NVIDIA's version of the component.
                v.map_err(|e| color_eyre::eyre::eyre!(e))?
            } else {
                // Probe PyPI first — the overwhelmingly common case — then
                // fall back to public-source-repository discovery for names
                // PyPI does not carry (standalone Rust crates, plain C/C++
                // libraries, or PyPI releases that ship no usable sdist).
                // Mirrors `resolve_dependency_config` so a bare CLI name
                // goes through the same resolution a runtime-discovered
                // dependency does, instead of hard-failing the moment PyPI
                // comes back empty.
                // Version-only probe, not fetch_latest_release: a wheel-only
                // release (common for vendor SDKs) has a legitimate
                // info.version but no sdist, and the full parser would reject
                // it before the repo-fallback path even got a chance to run.
                let pypi_version = pypi_latest_version(name, reporter.as_ref());
                if let Some(v) = pypi_version {
                    v
                } else {
                    let repos = ap_build::pipeline::find_source_repo_candidates(name);
                    let Some(repo_url) = repos.verified.first() else {
                        return Err(color_eyre::eyre::eyre!(
                            "{name} has no usable PyPI release and no \
                             discoverable public source repository. Pass \
                             --url, --source-url, or a repository URL as \
                             the positional argument to point it at the \
                             source."
                        ));
                    };
                    let v = ap_build::sources::latest_release(repo_url)
                        .map_err(|e| color_eyre::eyre::eyre!(e))?;
                    reporter.report(&Event::Progress {
                        step: None,
                        message: format!(
                            "{name}: not on PyPI, using discovered source \
                             repository {repo_url} (latest release {v})"
                        ),
                    });
                    *source_repo = Some(repo_url.clone());
                    v
                }
            }
        }
    })
}

/// The name and RPM name a build is filed under, and the repository
/// subprojects. A repository names itself: its own name wins over the guess.
fn resolve_names(
    name: &str,
    version: &str,
    source_repo: &Option<String>,
    reporter: &Arc<dyn Reporter + Send + Sync>,
) -> color_eyre::Result<(String, String, Vec<(String, String)>)> {
    Ok(match source_repo {
        None => (name.to_string(), ap_build::osc::rpm_name(name), Vec::new()),
        Some(repo) => {
            let probe = ap_build::sources::probe(repo, &version, name)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            // For monorepos the user-provided name IS the source package name;
            // the detected project_name is just one subproject and must not
            // override it. Only apply the detected name for single-project repos.
            let effective_name = if probe.subprojects.len() > 1 {
                name.to_string()
            } else {
                let declared = probe
                    .project_name
                    .clone()
                    .unwrap_or_else(|| name.to_string());
                if !declared.eq_ignore_ascii_case(name) {
                    reporter.report(&Event::Progress {
                        step: None,
                        message: format!(
                            "{repo} calls itself '{declared}', not '{name}' — using that"
                        ),
                    });
                }
                declared
            };
            if probe.subprojects.len() > 1 {
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "{repo} holds {} projects: {}",
                        probe.subprojects.len(),
                        probe
                            .subprojects
                            .iter()
                            .map(|(d, _)| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            let rpm = probe.ecosystem.rpm_name(&effective_name);
            (effective_name, rpm, probe.subprojects)
        }
    })
}

/// Queue the packages OBS already reports as unresolvable, failed or broken in
/// `project`, when `--sweep` asks for it. The root package itself is never
/// queued here (see the comment in the body).
fn seed_stuck_deps(
    project: &str,
    distro: &str,
    arch: &str,
    sweep: bool,
    explicit_target: Option<&str>,
    rpm_name: &str,
    pending_deps: &mut Vec<String>,
    stuck_seeds: &mut std::collections::HashSet<String>,
) -> color_eyre::Result<()> {
    if sweep && explicit_target.is_none() {
        // The root target itself must never enter this generic sweep: it
        // goes on to `resolve_dependency_config` just like any other
        // runtime-discovered dependency, which always re-fetches the
        // *latest* PyPI release and clears `source_repo` (see its doc
        // comment) — silently discarding this exact invocation's
        // `--version`/`--source-url`/`--url` and spawning a second,
        // wrongly-configured `run_package` thread racing the correctly
        // configured one `select_target` selects below, against the same
        // checkout and rpm name. OBS showing the root package itself as
        // Failed/Unresolvable/Broken from an earlier interrupted run is
        // exactly the common case this would trigger on.
        let root_key = ap_build::osc::fuzzy_key(&rpm_name);
        let already_stuck: Vec<String> =
            ap_build::osc::get_project_build_statuses(project, distro, arch)?
                .into_iter()
                .filter(|(_, status)| {
                    matches!(
                        status,
                        ap_build::osc::BuildStatus::Unresolvable
                            | ap_build::osc::BuildStatus::Failed
                            | ap_build::osc::BuildStatus::Broken
                    )
                })
                .map(|(pkg, _)| pkg)
                .filter(|pkg| ap_build::osc::fuzzy_key(pkg) != root_key)
                .collect();
        if !already_stuck.is_empty() {
            ap_core::diag!(
                "{} package(s) already unresolvable/failed/broken in {project}, queuing: {}",
                already_stuck.len(),
                already_stuck.join(", ")
            );
        }
        // Keyed by `fuzzy_key`, not the raw stripped name: OBS's real listing
        // and whatever spelling an agent later reports the same dependency
        // under (`pydantic_core` vs. the real `pydantic-core`, say) are not
        // guaranteed to agree on separators — see `osc::fuzzy_key`'s doc
        // comment. An exact-string seed set would silently fail to recognize
        // its own entries.
        stuck_seeds.extend(
            already_stuck
                .iter()
                .map(|pkg| ap_build::osc::fuzzy_key(pkg.strip_prefix("python-").unwrap_or(pkg))),
        );
        pending_deps.extend(already_stuck);
    }
    Ok(())
}
