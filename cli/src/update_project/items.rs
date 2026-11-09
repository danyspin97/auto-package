use super::*;
use crate::build_cmd::copy_build_flags;

pub(super) fn visit_key(item: &Item) -> String {
    format!("{}/{}", item.origin, osc::fuzzy_key(&item.pkg))
}

/// Why `item` cannot be built for `arch` by its own arch tags: the origin's
/// spec has an `ExclusiveArch:` without `arch`, or an `ExcludeArch:` with it.
/// `None` when it can be, or the origin's spec cannot be read (then the build
/// decides). Checked before the package is branched, so nothing is created for
/// a package that could only end `excluded`.
pub(super) fn arch_exclusion(item: &Item, arch: &str, ctx: &Ctx<'_>) -> Option<String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let spec = osc::cat_file(&item.origin, &item.pkg, &format!("{}.spec", item.pkg), emit)?;
    ap_build::spec::arch_excluded_reason(&spec, arch)
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
    rebase: bool,
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
            // A branch left by an earlier run may be behind its origin; one
            // that is continued from is ahead of it, and stays as it is.
            match rebase_unless_continuing(rebase, || osc::rebase_package(branch, &item.pkg, emit))
            {
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
                Err(e) => {
                    // Only a 404 that OBS confirms for the link's own origin
                    // (which may not be the plan's origin) stops the package.
                    let gone = if e.is_not_found() {
                        osc::missing_origin(branch, &item.pkg, emit)
                    } else {
                        None
                    };
                    match missing_in_origin(&item.pkg, &e, gone.as_ref().map(|o| o.project.as_str())) {
                        Some(why) => return Err(why),
                        None => ctx.warn(format!(
                            "could not rebase {branch}/{} onto its origin, building it as it is: {e}",
                            item.pkg
                        )),
                    }
                }
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

/// Why a package whose rebase got a 404 is not built: the 404 stops it only
/// when `origin_missing` names the link's origin, which OBS confirmed as
/// missing. Otherwise the 404 is a warning and the package is built as it is.
pub(super) fn missing_in_origin(
    pkg: &str,
    e: &osc::Error,
    origin_missing: Option<&str>,
) -> Option<String> {
    let origin = origin_missing.filter(|_| e.is_not_found())?;
    Some(format!(
        "{pkg} is not on OBS in {origin}: its rebase got a 404 and OBS confirms that \
         {origin} has no such package; it is not built or submitted, create it in {origin} first"
    ))
}

/// The result of `rebase` when the branch is to be rebased onto its origin;
/// otherwise `rebase` is not called, and there was nothing to rebase.
pub(super) fn rebase_unless_continuing(
    rebase: bool,
    run: impl FnOnce() -> Result<osc::Rebased, osc::Error>,
) -> Result<osc::Rebased, osc::Error> {
    if rebase {
        run()
    } else {
        Ok(osc::Rebased::NotALink)
    }
}

/// The Python flavors a spec's `%{python_module}` BuildRequires are looked
/// up for (`python313-<name>`): those of the work directory, else the host's
/// `%{pythons}`. Only `--update-deps` reads them, to choose dependencies, and
/// not with `--test-against-updates`, which keeps the dependencies it had.
/// None when no flavor is known: the macros are then not looked up.
pub(super) fn python_module_flavors(a: &UpdateProjectArgs, ctx: &Ctx<'_>) -> Vec<String> {
    if !a.update_deps || a.test_against_updates {
        return Vec::new();
    }
    let why = match ctx.python_flavors.try_effective() {
        Ok(flavors) if !flavors.is_empty() => return flavors,
        Ok(_) => "no Python flavor is known".to_string(),
        Err(e) => e,
    };
    ctx.say(format!(
        "--update-deps: {why}, so %{{python_module}} BuildRequires are not looked up"
    ));
    Vec::new()
}

/// `spec`'s BuildRequires, each `%{python_module}` read for `flavors` (see
/// [`depreach::expand_python_modules`]).
pub(super) fn spec_requirements(spec: &str, flavors: &[String]) -> depreach::PythonModules {
    depreach::expand_python_modules(&ap_build::spec::build_requirements(spec), flavors)
}

/// Check `item`'s BuildRequires `reqs` against the branch project's paths
/// and add the paths that are missing, though none for a name only a
/// `%{python_module}` gave. Returns the paths chosen and the verdicts.
pub(super) fn reach_dependencies(
    a: &UpdateProjectArgs,
    item: &Item,
    modules: &depreach::PythonModules,
    resolver: &DepResolver,
    run_packages: &[String],
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> color_eyre::Result<(Vec<PathAddition>, Vec<depreach::DepVerdict>)> {
    let reqs = &modules.requirements;
    let deps: Vec<ap_build::spec::BuildRequire> = reqs.iter().map(|r| r.dep.clone()).collect();
    let mut verdicts = resolver
        .check(&deps)
        .map_err(|e| eyre!("checking the BuildRequires of {}: {e}", item.pkg))?;
    verdicts.extend(
        resolver
            .check_requirements(reqs, &item.pkg, &item.origin, run_packages)
            .map_err(|e| eyre!("checking the capabilities of {}: {e}", item.pkg))?,
    );

    let mut unchecked: Vec<String> = Vec::new();
    let mut not_published: Vec<&str> = Vec::new();
    let mut outside: Vec<&str> = Vec::new();
    for v in &verdicts {
        match &v.state {
            DepState::Missing { .. } if modules.flavored.contains(&v.name) => outside.push(&v.name),
            DepState::OnlyUnusable { .. } => {
                // Published only where no path can point, which is not evidence
                // that it is missing: warn when the build path's repositories do
                // not list it either, once per name (D-20261102-34).
                if matches!(resolver.in_build_path(&v.name), Ok(false))
                    && resolver.first_report(&v.name)
                {
                    ctx.warn(format!(
                        "{}: BuildRequires {} is not listed by any repository of the branch's \
                         build path; the build will say if it is missing",
                        item.pkg, v.name
                    ));
                }
            }
            DepState::NotPublished => not_published.push(&v.name),
            DepState::Unchecked => unchecked.push(v.name.clone()),
            DepState::Capability(state) => match state {
                depreach::CapState::NoProvider { unknown, .. } => {
                    report_no_provider(item, &v.name, state, a, ctx, report);
                    unchecked.extend(unknown.iter().map(|why| format!("{} ({why})", v.name)));
                }
                depreach::CapState::Unchecked(why) => unchecked.push(format!("{} ({why})", v.name)),
                depreach::CapState::Provided { .. } => {}
            },
            _ => {}
        }
    }
    if !unchecked.is_empty() {
        // The first few say what was not checked and why; the rest are a count.
        const SHOWN: usize = 3;
        let mut listed = unchecked[..unchecked.len().min(SHOWN)].join("; ");
        if unchecked.len() > SHOWN {
            listed.push_str(&format!("; and {} more", unchecked.len() - SHOWN));
        }
        ctx.say(format!(
            "{}: {} BuildRequires not checked: {listed}; the build will say if one is missing",
            item.pkg,
            unchecked.len(),
        ));
    }
    if !not_published.is_empty() {
        ctx.say(format!(
            "{}: no project publishes {} (a virtual provide, or missing)",
            item.pkg,
            not_published.join(", ")
        ));
    }

    if !outside.is_empty() {
        ctx.say(format!(
            "{}: {} published only outside the build path; no path is added for a \
             %{{python_module}} BuildRequires",
            item.pkg,
            outside.join(", ")
        ));
    }
    let pathed: Vec<depreach::DepVerdict> = verdicts
        .iter()
        .filter(|v| !modules.flavored.contains(&v.name))
        .cloned()
        .collect();
    let adds = depreach::choose_paths(&pathed);
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

/// One warning per repository a provider in the paths does not satisfy the
/// requirement in, naming that provider and its version. Nothing is added to
/// the build paths for a capability.
fn report_no_provider(
    item: &Item,
    requirement: &str,
    state: &depreach::CapState,
    a: &UpdateProjectArgs,
    ctx: &Ctx<'_>,
    report: &mut Report,
) {
    let depreach::CapState::NoProvider { found, .. } = state else {
        return;
    };
    for f in found {
        ctx.warn(format!(
            "{}: {requirement} has no provider that satisfies it in {} ({}) (found: {} {})",
            item.pkg, f.repo, a.arch, f.rpm, f.version
        ));
        report.requirements.push(MissingRequirement {
            pkg: item.pkg.clone(),
            requirement: requirement.to_string(),
            repository: f.repo.clone(),
            arch: a.arch.clone(),
        });
    }
}

/// The dependencies of `parent` that live in another project and have a newer
/// upstream release, as items to branch from that project, each with what
/// becomes of it given `parent`'s requirements `reqs` and what `predict`
/// says its new release requires (see [`choose_dependency`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn dependency_items(
    a: &UpdateProjectArgs,
    branch: &str,
    parent: &Item,
    reqs: &[ap_build::spec::Requirement],
    verdicts: &[depreach::DepVerdict],
    adds: &[PathAddition],
    predict: &dyn Fn() -> depreach::Prediction,
    ctx: &Ctx<'_>,
    emit: Emit<'_>,
) -> Vec<(Item, DepChoice)> {
    let mut out = Vec::new();
    // Asked once, and only for a package that has a dependency to choose for.
    let prediction = std::cell::OnceCell::new();
    for origin in
        depreach::dependency_origins(verdicts, adds, branch, &a.source, !a.test_against_updates)
    {
        let (project, pkg) = (origin.project.clone(), origin.package.clone());
        // PEP 503 key: `python-whois` and `python-pythonwhois` are two projects.
        let key = osc::project_key(&pkg);
        let plan = update::plan_where(
            &project,
            &|n| osc::project_key(up::branch_name(n)) == key,
            emit,
        )
        .and_then(|candidates| -> Result<Vec<_>, String> {
            let names: Vec<&str> = candidates
                .iter()
                .map(|c| up::branch_name(&c.rpm_name))
                .collect();
            let spelling = listed_spelling(&pkg, &names)?.map(str::to_string);
            Ok(candidates
                .into_iter()
                .filter(|c| Some(up::branch_name(&c.rpm_name)) == spelling.as_deref())
                .collect())
        });
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
                    let choice = choose_dependency(
                        a.test_against_updates,
                        reqs,
                        prediction.get_or_init(predict),
                        &origin,
                        &item,
                        &parent.pkg,
                        branch,
                    );
                    out.push((item, choice));
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

/// Whether an item needs the agent and the run has none: an item with no
/// fixable finding goes to the pipeline anyway, because the pipeline applies
/// its proven removal of a merged request patch without a model.
pub(super) fn agent_needed(no_agent: bool, fixable: &[String]) -> bool {
    no_agent && !fixable.is_empty()
}
