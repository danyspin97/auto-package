use super::*;
use crate::build_cmd::copy_build_flags;

pub(super) fn visit_key(item: &Item) -> String {
    format!("{}/{}", item.origin, osc::fuzzy_key(&item.pkg))
}

/// Whether `package` is absent from the branch project's listing taken before
/// the branch call, so that call is what created it. A listing that could not
/// be read proves nothing, so the package is not treated as created.
pub(super) fn created_by_branch(
    before: &Result<Arc<HashSet<String>>, String>,
    package: &str,
) -> bool {
    match before {
        Ok(names) => !names.contains(package),
        Err(_) => false,
    }
}

/// Branch `item` into the branch project, or note that it is already there.
/// The item's origin becomes the project OBS says it was branched from.
pub(super) fn branch_item(
    branch: &str,
    item: &mut Item,
    follow_devel: bool,
    revision: Option<&str>,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    if !follow_devel
        && let Some(devel) = osc::package_devel_project(&item.origin, &item.pkg, emit)
        && devel != item.origin
    {
        ctx.warn(format!(
            "{} has the devel project {devel}; branching from {} as asked, so its request \
             goes there and skips {devel} (--follow-devel branches from the devel project)",
            item.pkg, item.origin
        ));
    }
    // What the branch project held before this call, so a package the call
    // created can be told apart from one that was already there.
    let packages_before = osc::try_list_project_packages(branch, emit);
    let result = osc::branch_package_at(
        &item.origin,
        &item.pkg,
        &item.pkg,
        branch,
        follow_devel,
        revision,
        emit,
    )
    .map_err(|e| e.to_string());
    // The project's package listing is cached; it has changed now.
    osc::invalidate_project_packages_cache(branch);
    match result? {
        BranchResult::Branched(b) => {
            if b.target_package != item.pkg {
                // The branch just created a package nothing will build or
                // submit; remove it rather than leave it in the project. A
                // package that was there before this call is not ours to delete.
                let cleanup = if !created_by_branch(&packages_before, &b.target_package) {
                    format!(
                        "; left {}/{} in place: it was there before this branch, or could not be checked",
                        b.target_project, b.target_package
                    )
                } else if b.target_project == branch {
                    match osc::delete_package(&b.target_project, &b.target_package, emit) {
                        Ok(()) => format!("; removed {}/{}", b.target_project, b.target_package),
                        Err(e) => format!(
                            "; could not remove {}/{}: {e}",
                            b.target_project, b.target_package
                        ),
                    }
                } else {
                    String::new()
                };
                osc::invalidate_project_packages_cache(branch);
                return Err(format!(
                    "OBS branched it as {}, not {}{cleanup}",
                    b.target_package, item.pkg
                ));
            }
            if b.origin_project != item.origin {
                ctx.warn(format!(
                    "{} was branched from {}, not {}; its request goes there",
                    item.pkg, b.origin_project, item.origin
                ));
            }
            ctx.say(format!(
                "branched {}/{}{} to {}/{}",
                b.origin_project,
                item.pkg,
                revision.map_or(String::new(), |r| format!(" at revision {r}")),
                b.target_project,
                b.target_package
            ));
            copy_build_flags(
                ctx.reporter.as_ref(),
                &b.target_project,
                &b.origin_project,
                &item.pkg,
                emit,
            );
            item.origin = b.origin_project;
        }
        BranchResult::AlreadyThere if revision.is_some() => {
            return Err(format!(
                "{branch} already has {}, which may hold another revision than {}; \
                 delete {branch}/{} (or pick another --branch-project) to branch that revision",
                item.pkg,
                revision.unwrap_or_default(),
                item.pkg
            ));
        }
        BranchResult::AlreadyThere => {
            ctx.say(format!("{branch} already has {}; reusing it", item.pkg));
            // A branch left by an earlier run may be behind its origin.
            match osc::rebase_package(branch, &item.pkg, emit) {
                Ok(osc::Rebased::NotALink) => {}
                Ok(osc::Rebased::Clean) => {
                    ctx.say(format!("rebased {branch}/{} onto its origin", item.pkg))
                }
                Ok(osc::Rebased::Conflict(why)) => {
                    return Err(format!(
                        "rebasing {branch}/{} onto its origin left a conflict ({why}); \
                         resolve it in OBS or delete the branch package",
                        item.pkg
                    ));
                }
                Err(e) => ctx.warn(format!(
                    "could not rebase {branch}/{} onto its origin, building it as it is: {e}",
                    item.pkg
                )),
            }
            // The flags come from the project the branch is linked from, which
            // is not always the plan's origin (a branch made from a devel
            // project, or by a run with --follow-devel).
            match osc::link_project(branch, &item.pkg, emit) {
                Ok(Some(link)) => copy_build_flags(
                    ctx.reporter.as_ref(),
                    branch,
                    &link,
                    &item.pkg,
                    emit,
                ),
                Ok(None) => copy_build_flags(
                    ctx.reporter.as_ref(),
                    branch,
                    &item.origin,
                    &item.pkg,
                    emit,
                ),
                Err(e) => ctx.warn(format!(
                    "could not read the link of {branch}/{}, so its build flags are not copied: {e}",
                    item.pkg
                )),
            }
        }
    }
    Ok(())
}

/// Check `item`'s BuildRequires against the branch project's paths and add
/// the paths that are missing. Returns the paths chosen and the verdicts.
pub(super) fn reach_dependencies(
    a: &UpdateProjectArgs,
    item: &Item,
    spec: &str,
    resolver: &DepResolver,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> color_eyre::Result<(Vec<PathAddition>, Vec<depreach::DepVerdict>)> {
    let deps = ap_build::spec::build_requires(spec);
    let verdicts = resolver
        .check(&deps)
        .map_err(|e| eyre!("checking the BuildRequires of {}: {e}", item.pkg))?;

    let mut unchecked = 0;
    let mut not_published: Vec<&str> = Vec::new();
    for v in &verdicts {
        match &v.state {
            DepState::OnlyUnusable { hits } => {
                let projects: Vec<&str> = hits.iter().map(|h| h.project.as_str()).collect();
                ctx.warn(format!(
                    "{}: BuildRequires {} is only published in {}, which cannot be added as a \
                     build path (a personal project, or no repository matching the branch's)",
                    item.pkg,
                    v.name,
                    projects.join(", ")
                ));
            }
            DepState::NotPublished => not_published.push(&v.name),
            DepState::Unchecked => unchecked += 1,
            _ => {}
        }
    }
    if unchecked > 0 {
        ctx.say(format!(
            "{}: {unchecked} capability-style BuildRequires (pkgconfig(), %{{...}}) not checked; \
             the build will say if one is missing",
            item.pkg
        ));
    }
    if !not_published.is_empty() {
        ctx.say(format!(
            "{}: no project publishes {} (a virtual provide, or missing)",
            item.pkg,
            not_published.join(", ")
        ));
    }

    let adds = depreach::choose_paths(&verdicts);
    // A maintenance run adds no build path: a package that needs one fails
    // its code stream instead (the incident could not build it).
    if a.maintenance_source {
        let wanted: Vec<String> = adds
            .iter()
            .map(|p| format!("{}/{}", p.project, p.path_repository))
            .collect();
        if let Some(why) = ap_build::maintenance::build_path_refusal(&item.pkg, &wanted) {
            return Err(eyre!(why));
        }
    }
    if a.dry_run {
        for p in &adds {
            if !report.paths.contains(p) {
                ctx.say(format!(
                    "{}: would add build path {}/{} (repository {})",
                    item.pkg, p.project, p.path_repository, p.repository
                ));
                report.paths.push(p.clone());
            }
        }
    } else {
        let made = resolver
            .add_paths(&adds)
            .map_err(|e| eyre!("adding build paths for {}: {e}", item.pkg))?;
        report.paths.extend(made);
    }
    Ok((adds, verdicts))
}

/// The dependencies of `parent` that live in another project and have a newer
/// upstream release, as items to branch from that project.
pub(super) fn dependency_items(
    a: &UpdateProjectArgs,
    branch: &str,
    parent: &Item,
    verdicts: &[depreach::DepVerdict],
    adds: &[PathAddition],
    ctx: &Ctx<'_>,
    emit: Emit<'_>,
) -> Vec<Item> {
    let mut out = Vec::new();
    for (project, pkg) in depreach::dependency_origins(verdicts, adds, branch, &a.source) {
        let key = osc::fuzzy_key(&pkg);
        let plan = update::plan_where(
            &project,
            &|n| osc::fuzzy_key(up::branch_name(n)) == key,
            emit,
        );
        match plan {
            Ok(candidates) => {
                for mut item in up::items_from_plan(&project, &candidates, &a.exclude) {
                    if up::check_pipeline_name(&item).is_err() {
                        ctx.warn(format!(
                            "not updating dependency {} of {}: the pipeline would name it differently",
                            item.pkg, parent.pkg
                        ));
                        continue;
                    }
                    item.dependency_of = Some(parent.pkg.clone());
                    out.push(item);
                }
            }
            Err(e) => ctx.warn(format!(
                "could not check dependency {pkg} of {} in {project}: {e}",
                parent.pkg
            )),
        }
    }
    out
}

/// Build what is ready in the branch project, the dependencies before what
/// needs them, each like `update` builds in its own project.
pub(super) fn build_ready(
    a: &UpdateProjectArgs,
    branch: &str,
    ready: &mut Vec<Item>,
    mechanical: &HashSet<String>,
    resolver: &Arc<DepResolver>,
    ctx: &Ctx<'_>,
    report: &mut Report,
) {
    ready.sort_by_key(|i| i.dependency_of.is_none());
    for item in std::mem::take(ready) {
        if let Some(b) = &ctx.backend {
            b.register(&item);
        }
        let done = if let Some(job) = &a.backport_job {
            ctx.say(format!(
                "backporting into {} {} in {branch} (the version stays)",
                item.pkg, item.new_version
            ));
            stage_backport(branch, &item, job, ctx)
                .and_then(|()| build_item(a, branch, &item, resolver, ctx))
        } else if a.improve {
            ctx.say(format!("improving {} in {branch}", item.pkg));
            if mechanical.contains(&item.pkg) {
                tidy_item(branch, &item, ctx)
            } else if a.no_agent {
                report.results.push(ItemResult {
                    item,
                    outcome: ItemOutcome::Skipped("the findings need the agent".to_string()),
                    sr: SrOutcome::NotRequested,
                });
                continue;
            } else {
                build_item(a, branch, &item, resolver, ctx)
            }
        } else {
            ctx.say(format!(
                "updating {} to {} in {branch}",
                item.pkg, item.new_version
            ));
            build_item(a, branch, &item, resolver, ctx)
        };
        let outcome = match done {
            Ok(()) => ItemOutcome::Updated,
            Err(why) => ItemOutcome::Failed(why),
        };
        report.results.push(ItemResult {
            item,
            outcome,
            sr: SrOutcome::NotRequested,
        });
    }
}

/// Build `item` in the branch project with the ordinary pipeline.
fn build_item(
    a: &UpdateProjectArgs,
    branch: &str,
    item: &Item,
    resolver: &Arc<DepResolver>,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    // As in `update`: a PyPI package is named by its PyPI name; a repository
    // one by the repository, which is also handed back so the pipeline fetches
    // from the forge instead of guessing a PyPI release.
    let (name, source_repo) = match &item.ecosystem {
        Ecosystem::Pypi { pypi_name } => (pypi_name.clone(), None),
        Ecosystem::GitRepo { url } => (repo_package_name(url), Some(url.clone())),
        // A release directory: the build gets the address of the release it packages.
        Ecosystem::Index { url } => (
            item.pkg.clone(),
            Some(url.replace(&item.old_version, &item.new_version)),
        ),
        Ecosystem::Repology { .. } => {
            return Err(format!(
                "{}'s source is known only through the {} lookup, which names nothing the pipeline \
                 can fetch and unpack yet",
                item.pkg,
                item.ecosystem.tag()
            ));
        }
    };
    let build = BranchBuild {
        rpm_name: Some(item.pkg.clone()),
        resolver: Some(resolver.clone()),
        improve: a
            .backport_job
            .as_ref()
            .map(|job| job.problem(&item.pkg, &item.new_version))
            .or_else(|| improve_problem(item)),
        from_version: Some(item.old_version.clone()),
        // A maintenance run (its code streams and backports) keeps its own
        // recipe rules and is not an update of the package.
        update: !a.improve && !a.maintenance_source && a.backport_job.is_none(),
    };
    let distro = crate::choose_distro(a.distro.as_deref(), branch, &a.arch);
    catch_panic(|| {
        cmd_build(
            &name,
            Some(&item.new_version),
            branch,
            &ctx.work_dir,
            &ctx.state_root,
            &ctx.rpms_dir,
            // The branch took its repositories from the project it was
            // branched from, which may call the distribution by another name.
            &distro,
            &a.arch,
            a.max_attempts,
            a.budget_usd,
            None,
            true, // force: a rebuild at the new version must not be skipped as built
            a.regen,
            a.no_agent,
            ctx.sem.clone(),
            false, // sweep: updating X must not restart every other stuck package
            source_repo,
            ctx.source_urls.clone(),
            ctx.python_flavors.clone(),
            ctx.run_dir,
            ctx.reporter.clone(),
            ctx.base_reporter.clone(),
            ctx.tui_ctrl.clone(),
            Some(&build),
        )
    })
    .map_err(|e| format!("{e:#}"))
}
