use super::*;

/// What the dependency queue reads and changes: the build's context, the
/// queue's state, and the records of the run.
pub(super) struct Build<'a> {
    pub(super) project: &'a str,
    pub(super) distro: &'a str,
    pub(super) arch: &'a str,
    pub(super) rpm_name: &'a str,
    pub(super) version: &'a str,
    pub(super) cover_flavors: &'a [String],
    pub(super) branch: Option<&'a update_project::BranchBuild>,
    pub(super) state_root: &'a Path,
    pub(super) reporter: &'a Arc<dyn Reporter + Send + Sync>,
    pub(super) pkg_reporter: &'a dyn Fn(&str) -> Arc<dyn Reporter + Send + Sync>,
    pub(super) tui_ctrl: &'a Option<tui::TuiController>,
    pub(super) sem: &'a Option<Semaphore>,
    pub(super) run_dir: &'a RunDir,
    pub(super) dep_drops: &'a DepDrops,
    pub(super) patch_requests: &'a PatchRequests,
    pub(super) native_names: &'a NativeNames,
    pub(super) outdated_names: &'a OutdatedNames,
    pub(super) attempted_deps: &'a mut std::collections::HashSet<String>,
    pub(super) deferred_once: &'a mut std::collections::HashSet<String>,
    pub(super) retrying: &'a mut std::collections::HashSet<String>,
    pub(super) dep_rounds: &'a mut std::collections::HashMap<String, usize>,
    pub(super) stuck_seeds: &'a mut std::collections::HashSet<String>,
    pub(super) pending_deps: &'a mut Vec<String>,
    pub(super) config: &'a mut PipelineConfig,
    pub(super) max_dep_attempts: usize,
}

/// Builds the dependencies queued so far, and those they bring with them,
/// until the queue is empty.
pub(super) fn drain_dep_queue(b: Build<'_>) -> color_eyre::Result<()> {
    let Build {
        project,
        distro,
        arch,
        cover_flavors,
        branch,
        state_root,
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
        deferred_once,
        retrying,
        dep_rounds,
        stuck_seeds,
        pending_deps,
        config,
        max_dep_attempts,
        ..
    } = b;
    let mut retry_stack: Vec<Vec<String>> = Vec::new();
    while !pending_deps.is_empty() || !retry_stack.is_empty() {
        if pending_deps.is_empty() {
            *pending_deps = retry_stack.pop().unwrap_or_default();
        }
        let batch = std::mem::take(pending_deps);
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
        for (path_project, _repo) in ap_build::osc::get_project_repo_paths(project, lookup_emit)? {
            let listing = ap_build::osc::try_list_project_packages(&path_project, lookup_emit)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            linked_listings.push((path_project, listing));
        }
        // What a dependency build runs as: its own thread, reported as soon
        // as it is done. Reporting in join order let a slower sibling hold
        // back every package finished after it, so their rows stayed on
        // their last step.
        let dep_spawner = DepSpawner {
            run_dir,
            reporter: &reporter,
            tui_ctrl: &tui_ctrl,
            pkg_reporter: &pkg_reporter,
            sem: &sem,
            dep_drops: &dep_drops,
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
                    if let Some(h) = dep_spawner.spawn(dep_config, dep.clone()) {
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
                    if let Some(h) = dep_spawner.spawn(dep_config, dep.clone()) {
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
            if ap_build::osc::project_ships_binary(project, arch, dep, lookup_emit) != Some(false) {
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
                        let blockers: Vec<String> =
                            ap_build::osc::unresolvable_blockers(project, real_pkg, distro, arch)
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
                dep_spawner.spawn(dep_config, dep)
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

        DepLedger {
            attempted_deps,
            dep_rounds,
            retrying,
            pending_deps,
            retry_group: &mut retry_group,
            config,
            patch_requests: &patch_requests,
            native_names: &native_names,
            outdated_names: &outdated_names,
            dep_drops: &dep_drops,
            pkg_reporter: &pkg_reporter,
            max_dep_attempts,
        }
        .record(handles);
        if ap_build::proc::abort_reason().is_some() {
            return Err(color_eyre::eyre::eyre!("run aborted"));
        }
        if !retry_group.is_empty() {
            retry_stack.push(retry_group);
        }
    }
    Ok(())
}
