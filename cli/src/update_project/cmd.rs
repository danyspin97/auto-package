use super::*;

pub(super) fn cmd_update_project(
    a: &UpdateProjectArgs,
    branch: &str,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> color_eyre::Result<()> {
    let emit = Emit::run(ctx.reporter.as_ref());

    // The source is read to plan and to copy the repositories of a branch
    // project that does not exist yet; a maintenance project is not something
    // this tool branches from.
    let source_meta = osc::get_project_meta(&a.source, emit)
        .map_err(|e| eyre!("could not read the meta of {}: {e}", a.source))?
        .ok_or_else(|| eyre!("OBS project {} does not exist", a.source))?;
    let source_info = osc::parse_project_info(&source_meta);
    if let Some(kind) = &source_info.kind
        && !(a.maintenance_source && ap_build::maintenance::source_kind_allowed(kind))
    {
        return Err(eyre!(
            "{} is a {kind} project; branching from maintenance projects is not supported",
            a.source
        ));
    }
    let branch_meta = osc::get_project_meta(branch, emit)
        .map_err(|e| eyre!("could not read the meta of {branch}: {e}"))?;
    if let Some(meta) = &branch_meta {
        let info = osc::parse_project_info(meta);
        if info.locked {
            return Err(eyre!(
                "branch project {branch} is locked; unlock it or pick another"
            ));
        }
        if let Some(kind) = info.kind {
            return Err(eyre!("branch project {branch} is a {kind} project"));
        }
    }

    // A dry run reads the branch project as branching would leave it.
    let resolver = {
        let live = LiveObs {
            reporter: ctx.reporter.clone(),
        };
        let resolver_for = |obs: Box<dyn depreach::Obs>| {
            DepResolver::new(obs, ctx.reporter.clone(), branch, &a.arch)
        };
        let resolver = if a.dry_run && !a.in_place {
            resolver_for(Box::new(DryRunObs {
                inner: live,
                branch_project: branch.to_string(),
                branch_meta: depreach::synthetic_branch_meta(branch, &a.source, &source_info),
            }))
        } else {
            resolver_for(Box::new(live))
        };
        // An in-place run's branch is its source, which keeps its own rebuild modes.
        let resolver = if a.in_place {
            resolver.for_in_place_source()
        } else {
            resolver
        };
        // A maintenance incident builds against the code stream only: its
        // branch gets no build path that the incident would not have.
        if a.maintenance_source {
            resolver.with_frozen_paths(ap_build::maintenance::FROZEN_PATHS_REASON)
        } else {
            resolver
        }
    };
    let resolver = Arc::new(resolver);

    // Each package is built on its own thread as soon as it is prepared, at
    // most `--jobs` at a time; the run goes on preparing the next meanwhile,
    // and branches it only while the throttle has room.
    let throttle = Throttle::new(a.jobs, ap_build::proc::shutting_down);
    let work = BuildWork::new(a, branch, &resolver, ctx, &throttle);
    let mut built = Vec::new();
    let prepared = std::thread::scope(|s| {
        let mut builds = Scheduler::new(s, &work, a.jobs);
        let prepared = prepare(
            a,
            branch,
            branch_meta.is_some(),
            &resolver,
            ctx,
            &mut builds,
            report,
        );
        built = builds.finish();
        prepared
    });
    merge_built(report, built);
    if prepared? && a.submit {
        submit_requests(a, branch, &mut report.results, ctx)?;
    }
    Ok(())
}

/// Find the packages to work on, prepare each and hand it over to `builds`.
/// `Ok(true)` when the run got to the end of its packages and may submit.
fn prepare(
    a: &UpdateProjectArgs,
    branch: &str,
    branch_exists: bool,
    resolver: &Arc<DepResolver>,
    ctx: &Ctx<'_>,
    builds: &mut Builds<'_, '_, '_, '_>,
    report: &mut Report,
) -> color_eyre::Result<bool> {
    let emit = Emit::run(ctx.reporter.as_ref());
    // `update` sweeps the source for newer releases and `lint` audits it, each
    // on its own thread. A package that has a release, or findings, is branched
    // and built as it turns up, while the thread goes on with the others.
    let mut mechanical: HashSet<String> = HashSet::new();
    let (mut sweep, mut audit) = (None, None);
    // Whether the sweep queues each update's row when it finds it.
    let mut sweep_queues = false;
    // A maintenance backport or a bug fix builds each package at the version it
    // has: no sweep, the items come straight from the source's specs.
    let backports: Option<std::cell::RefCell<VecDeque<Item>>> =
        if a.backport_job.is_some() || a.fix_task.is_some() {
            Some(std::cell::RefCell::new(
                backport_items(a, emit).map_err(|e| eyre!(e))?.into(),
            ))
        } else {
            None
        };
    if backports.is_some() {
        // The items are queued above.
    } else if a.improve {
        audit = Some(Audit::start(a, branch, ctx.reporter.clone()));
    } else if a.revision.is_some() || a.to_version.is_some() {
        sweep = Some(Sweep::start_pinned(
            &a.source,
            &a.only,
            a.revision.clone(),
            a.to_version.clone(),
            ctx.reporter.clone(),
        ));
    } else {
        // A dry run queues nothing: its rows stay as the check found them.
        let queue = (!a.dry_run).then(|| tui_row::tui_only(ctx)).flatten();
        sweep_queues = queue.is_some();
        sweep = Some(Sweep::start(
            &a.source,
            &a.only,
            &a.exclude,
            ctx.reporter.clone(),
            queue,
        ));
    }
    // The next package to work on, in the order it was found, and whether its
    // findings are only style slips, which are committed without a build.
    let incoming = || -> Option<(Item, bool)> {
        if let Some(queue) = &backports {
            return queue.borrow_mut().pop_front().map(|i| (i, false));
        }
        if let Some(audit) = &audit {
            return audit.items.recv().ok();
        }
        let sweep = sweep.as_ref()?;
        loop {
            let candidate = sweep.updates.recv().ok()?;
            let items =
                up::items_from_plan(&a.source, std::slice::from_ref(&candidate), &a.exclude);
            if let Some(item) = items.into_iter().next() {
                return Some((item, false));
            }
        }
    };
    // The rows the sweep queued, until a build or the preparation closes them,
    // and how many results the preparation has had closed so far.
    let mut sweep_rows: HashSet<String> = HashSet::new();
    let mut closed_upto = report.results.len();

    // Branch, then make each package's BuildRequires reachable. With
    // `--update-deps` a dependency from another project that has a newer
    // release is queued behind the package that needs it.
    let mut queue: VecDeque<Item> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    // The packages handled so far, which a capability may be provided by.
    let mut run_packages: Vec<String> = Vec::new();
    let mut ready: Vec<Job> = Vec::new();
    let mut admitted = 0;
    let mut dependency_count = 0;
    let mut branch_checked = false;
    let mut open_requests = requested::OpenRequests::default();
    loop {
        tui_row::close_dropped(ctx, &report.results, &mut closed_upto, &mut sweep_rows);
        let mut item = match queue.pop_front() {
            Some(item) => item,
            None => {
                // The package and what it needs are all prepared: build them
                // now, then wait for the sweep to find the next.
                if !a.dry_run {
                    hand_over(&mut ready, &mechanical, builds, report);
                }
                let Some((item, style_only)) = incoming() else {
                    break;
                };
                if sweep_queues && let Some(row) = row_name(&item) {
                    sweep_rows.insert(row);
                }
                if style_only {
                    mechanical.insert(item.pkg.clone());
                }
                if !visited.insert(visit_key(&item)) {
                    continue;
                }
                match up::check_pipeline_name(&item) {
                    // A style commit does not run the pipeline, so its naming does not matter.
                    _ if mechanical.contains(&item.pkg) => {}
                    Ok(()) => {}
                    Err(why) => {
                        ctx.warn(format!("skipping {}: {why}", item.pkg));
                        report.results.push(ItemResult {
                            item,
                            outcome: ItemOutcome::Skipped(why),
                            sr: SrOutcome::NotRequested,
                        });
                        continue;
                    }
                }
                admitted += 1;
                item
            }
        };
        // The name it was found under, which the items that need it wait for.
        let first_key = visit_key(&item);
        let mut after: Vec<String> = Vec::new();
        // A package that its own ExclusiveArch or ExcludeArch keeps off this
        // architecture would only end `excluded`: it is skipped before any branch.
        if let Some(why) = arch_exclusion(&item, &a.arch, ctx) {
            ctx.warn(format!("skipping {}: {why}", item.pkg));
            report.results.push(ItemResult {
                item,
                outcome: ItemOutcome::Skipped(why),
                sr: SrOutcome::NotRequested,
            });
            continue;
        }
        // A branch that holds upstream's version and built it is listed, not
        // rebuilt; one ahead of upstream is skipped. Both are decided before
        // the branch call, which would rebase the branch onto its origin.
        if existing::settled_in_branch(&item, branch, a, ctx, report) {
            continue;
        }
        // A lint of a branch an earlier update left ahead of its origin
        // continues from it: the branch call must not rebase it.
        let Some(continuing) =
            existing_lint::continue_from_branch(&mut item, branch, a, ctx, report)
        else {
            continue;
        };
        if a.in_place {
            // The package is already where it will be changed, and its
            // BuildRequires resolve: it builds there today.
            ready.push(Job::new(item, first_key, after));
            continue;
        }
        // An origin whose spec cannot be read cannot be updated: it is reported,
        // not branched, in the dry run as well.
        if origin::settled_as_broken_origin(&item, ctx, report) {
            continue;
        }
        if !a.dry_run {
            // An origin that is unresolvable in every repository gives a branch
            // that cannot build either: it is reported, not branched.
            match osc::origin_unresolvable(&item.origin, &item.pkg) {
                Ok(Some(why)) => {
                    let why = format!("broken outside the tool: origin unresolvable ({why})");
                    ctx.warn(format!("{}: {why}", item.pkg));
                    report.results.push(ItemResult {
                        item,
                        outcome: ItemOutcome::Skipped(why),
                        sr: SrOutcome::NotRequested,
                    });
                    continue;
                }
                Ok(None) => {}
                Err(e) => ctx.warn(format!(
                    "{}: could not read the origin's results, branching anyway: {e}",
                    item.pkg
                )),
            }
        }
        // A package that an open submit request already updates is listed, not
        // branched: the request is read once per origin, for all its packages.
        // A lint continued from the branch is not checked: its version is the
        // branch's, so no request can bring an update to it.
        if !continuing && requested::settled_as_requested(&item, a, &mut open_requests, ctx, report)
        {
            continue;
        }
        // A package whose openSUSE:Factory copy comes from another devel project
        // is left to the maintainer, unless `--deprecated` asks for it. It is
        // read from Factory last, after every check that skips without a request.
        if deprecated::settled_as_deprecated(&item, a, ctx, report) {
            continue;
        }
        if !a.dry_run {
            // Branching queues a build in the branch project: it waits until
            // fewer than `2 x jobs` items are handed over and not built.
            if !builds.work().throttle.admit() {
                let why = "interrupted before it was branched".to_string();
                ctx.warn(format!("{}: {why}", item.pkg));
                report.results.push(ItemResult {
                    item,
                    outcome: ItemOutcome::Failed(why),
                    sr: SrOutcome::NotRequested,
                });
                continue;
            }
            match branch_item(
                branch,
                &mut item,
                a.follow_devel,
                a.revision.as_deref(),
                // A findings fix keeps the revision its review read: no rebase.
                !continuing
                    && !matches!(
                        a.fix_task.as_deref(),
                        Some(ap_build::bugfix::FixTask::Findings(_))
                    ),
                ctx,
            ) {
                Ok(()) => {
                    // Branching may have changed the origin, so the item is
                    // also recorded under its new key: a later discovery of the
                    // same package from that project is then not processed twice.
                    visited.insert(visit_key(&item));
                }
                Err(why) => {
                    builds.work().throttle.dropped();
                    ctx.warn(format!("{}: {why}", item.pkg));
                    report.results.push(ItemResult {
                        item,
                        outcome: ItemOutcome::Failed(why),
                        sr: SrOutcome::NotRequested,
                    });
                    continue;
                }
            }
            if !branch_checked {
                // The project exists now, whether it was created or not.
                match osc::project_has_repositories(branch, emit) {
                    Ok(true) => branch_checked = true,
                    Ok(false) => {
                        builds.work().throttle.dropped();
                        let why = format!(
                            "branch project {branch} has no build repositories; configure one \
                             in its meta before building"
                        );
                        ctx.warn(format!("{}: {why}", item.pkg));
                        report.results.push(ItemResult {
                            item,
                            outcome: ItemOutcome::Failed(why),
                            sr: SrOutcome::NotRequested,
                        });
                        continue;
                    }
                    Err(e) => {
                        builds.work().throttle.dropped();
                        let why = format!("could not read the meta of {branch}: {e}");
                        ctx.warn(format!("{}: {why}", item.pkg));
                        report.results.push(ItemResult {
                            item,
                            outcome: ItemOutcome::Failed(why),
                            sr: SrOutcome::NotRequested,
                        });
                        continue;
                    }
                }
            }
        }

        // The origin is only read, so what the plan fetched from it is reused;
        // the branch, and a project changed in place, are read fresh.
        let origin_spec = |item: &Item| {
            if a.in_place {
                update::spec_for_package(&item.origin, &item.pkg, emit).ok()
            } else {
                update::planned_spec(&item.origin, &item.pkg, emit).ok()
            }
        };
        let spec = if a.dry_run {
            origin_spec(&item)
        } else {
            update::spec_for_package(branch, &item.pkg, emit)
                .ok()
                .or_else(|| origin_spec(&item))
        };
        match spec {
            Some(spec) => {
                if !run_packages.contains(&item.pkg) {
                    run_packages.push(item.pkg.clone());
                }
                let (adds, verdicts) =
                    match reach_dependencies(a, &item, &spec, resolver, &run_packages, ctx, report)
                    {
                        Ok(found) => found,
                        Err(e) => {
                            let why = e.to_string();
                            ctx.warn(format!("{}: {why}", item.pkg));
                            report.results.push(ItemResult {
                                item,
                                outcome: ItemOutcome::Failed(why),
                                sr: SrOutcome::NotRequested,
                            });
                            continue;
                        }
                    };
                if a.update_deps {
                    let reqs = ap_build::spec::build_requirements(&spec);
                    let mut deps = DepQueue {
                        queue: &mut queue,
                        visited: &mut visited,
                        count: &mut dependency_count,
                    };
                    let found =
                        dependency_items(a, branch, &item, &reqs, &verdicts, &adds, ctx, emit);
                    for (dep, choice) in found {
                        deps.add(&mut item, dep, choice, &mut after, ctx.reporter.as_ref());
                    }
                }
            }
            None => ctx.warn(format!(
                "could not read the spec of {}, so its BuildRequires are not checked",
                item.pkg
            )),
        }
        ready.push(Job::new(item, first_key, after));
    }
    tui_row::close_dropped(ctx, &report.results, &mut closed_upto, &mut sweep_rows);

    if let Some(sweep) = sweep.take() {
        let plan = match sweep.finish() {
            Ok(plan) => plan,
            Err(e) => {
                // Packages already branched and prepared are still built before the error is returned.
                if !a.dry_run {
                    hand_over(&mut ready, &mechanical, builds, report);
                }
                return Err(eyre!("{e}"));
            }
        };
        report.maintainer.extend(plan.maintainer.iter().cloned());
        for name in a.only.iter().filter(|n| !plan.names.contains(n)) {
            ctx.warn(format!("{name}: matched no package"));
        }
        if !plan.unreadable.is_empty() {
            // Packages already branched and prepared are still built before the error is returned.
            if !a.dry_run {
                hand_over(&mut ready, &mechanical, builds, report);
            }
            return Err(obs_unavailable(&plan.unreadable, &a.source));
        }
        if plan.total() == 0 {
            ctx.say(format!("no matching packages in {}", a.source));
            return none_checked(&a.only, false).map(|()| false);
        }
        if plan.only_absent() {
            return none_checked(&a.only, true).map(|()| false);
        }
        if plan.n_update == 0 {
            ctx.say("nothing to update");
            return Ok(false);
        }
    }
    if let Some(audit) = audit.take() {
        let end = match audit.finish() {
            Ok(end) => end,
            Err(e) => {
                // Packages already branched and prepared are still built before the error is returned.
                if !a.dry_run {
                    hand_over(&mut ready, &mechanical, builds, report);
                }
                return Err(eyre!("{e}"));
            }
        };
        for name in a.only.iter().filter(|n| !end.matched.contains(n)) {
            ctx.warn(format!("{name}: matched no package"));
        }
        if !end.unreadable.is_empty() {
            // Packages already branched and prepared are still built before the error is returned.
            if !a.dry_run {
                hand_over(&mut ready, &mechanical, builds, report);
            }
            return Err(obs_unavailable(&end.unreadable, &a.source));
        }
        // A named package that is skipped on purpose (no spec, a maintainer's
        // call) is a package found: only a name that matched nothing fails.
        if end.packages == 0
            && let Some(absent) = nothing_found(end.candidates, end.absent)
        {
            ctx.say(format!("no matching packages in {}", a.source));
            return none_checked(&a.only, absent).map(|()| false);
        }
        if end.items == 0 {
            ctx.say("nothing to improve");
            return Ok(false);
        }
    }
    if admitted == 0 {
        return Ok(false);
    }

    if a.dry_run {
        let items: Vec<Item> = ready.into_iter().map(|j| j.item).collect();
        say_dry_run(a, branch, branch_exists, &items, &mechanical, ctx, report);
        return Ok(false);
    }

    hand_over(&mut ready, &mechanical, builds, report);
    Ok(true)
}

/// What a run that checked no package ends with: an error when `--only` named
/// packages (they were asked for and none could be checked), else nothing.
fn none_checked(only: &[String], absent: bool) -> color_eyre::Result<()> {
    if only.is_empty() {
        return Ok(());
    }
    let why = if absent { "not on OBS: " } else { "" };
    Err(eyre!(
        "none of the requested packages could be checked: {why}{}",
        only.join(", ")
    ))
}

#[cfg(test)]
mod none_checked_tests {
    use super::none_checked;

    #[test]
    fn a_run_that_checked_nothing_fails_only_when_packages_were_asked_for() {
        assert!(none_checked(&[], false).is_ok());
        let err = none_checked(&["foo".to_string(), "bar".to_string()], false).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("none of the requested packages could be checked"),
            "{err}"
        );
        let absent = none_checked(&["gone".to_string()], true)
            .unwrap_err()
            .to_string();
        assert_eq!(
            absent,
            "none of the requested packages could be checked: not on OBS: gone"
        );
    }
}
