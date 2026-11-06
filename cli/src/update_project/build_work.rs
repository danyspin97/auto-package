//! The build of each prepared item, run by the scheduler on its own thread.

use super::schedule::{Scheduler, Work, interleave};
use super::*;
use std::sync::Mutex;

/// A prepared item, as it is handed to the scheduler.
#[derive(Clone)]
pub(super) struct Job {
    pub(super) item: Item,
    /// Its findings are style slips only, committed without a build.
    pub(super) mechanical: bool,
    /// The names the item is known by (see `visit_key`), before and after
    /// the branch call, which may change its origin.
    pub(super) keys: Vec<String>,
    /// The names of the dependencies that are built before it.
    pub(super) after: Vec<String>,
}

impl Job {
    pub(super) fn new(item: Item, first_key: String, after: Vec<String>) -> Self {
        let mut keys = vec![first_key];
        let key = visit_key(&item);
        if !keys.contains(&key) {
            keys.push(key);
        }
        Job {
            item,
            mechanical: false,
            keys,
            after,
        }
    }
}

/// The scheduler of a run's builds.
pub(super) type Builds<'scope, 'env, 'a, 'c> = Scheduler<'scope, 'env, BuildWork<'a, 'c>>;

/// Builds an item in the branch project, each like `update` builds in its
/// own project. Its output is the item's part of the report.
pub(super) struct BuildWork<'a, 'c> {
    a: &'a UpdateProjectArgs,
    branch: &'a str,
    resolver: &'a Arc<DepResolver>,
    ctx: &'a Ctx<'c>,
    /// Held by the other-architecture check, one at a time, when it edits the
    /// branch project's meta (see `edits_branch_meta`): two such checks at
    /// once would both rewrite it.
    arch_check: Mutex<()>,
}

impl<'a, 'c> BuildWork<'a, 'c> {
    pub(super) fn new(
        a: &'a UpdateProjectArgs,
        branch: &'a str,
        resolver: &'a Arc<DepResolver>,
        ctx: &'a Ctx<'c>,
    ) -> Self {
        BuildWork {
            a,
            branch,
            resolver,
            ctx,
            arch_check: Mutex::new(()),
        }
    }

    fn build(&self, job: Job, report: &mut Report) {
        let (a, branch, ctx, resolver) = (self.a, self.branch, self.ctx, self.resolver);
        let item = job.item;
        // Why the item is not submitted once its work is done (see `skipped`).
        let mut skip: Option<String> = None;
        let done = if let Some(backport) = &a.backport_job {
            ctx.say(format!(
                "backporting into {} {} in {branch} (the version stays)",
                item.pkg, item.new_version
            ));
            stage_backport(branch, &item, backport, ctx)
                .and_then(|()| build_item(a, branch, &item, resolver, ctx))
        } else if let Some(task) = &a.fix_task {
            ctx.say(format!(
                "fixing a bug in {} {} in {branch} (the version stays)",
                item.pkg, item.new_version
            ));
            build_item(a, branch, &item, resolver, ctx)
                .and_then(|()| super::fix_landed::check(branch, &item, task, ctx))
        } else if a.improve {
            ctx.say(format!("improving {} in {branch}", item.pkg));
            if job.mechanical {
                let target = osc::choose_repository(branch, a.distro.as_deref(), &a.arch).0;
                let ldconfig = ap_build::spec_tidy::hygiene::ldconfig_macro_defined(&target);
                match tidy_item(branch, &item, ctx, ldconfig) {
                    Ok(done) => {
                        skip = super::tidy::tidy_skip(done).map(str::to_string);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else if agent_needed(a.no_agent, &item.findings) {
                skipped(ctx, report, item, "the findings need the agent".to_string());
                return;
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
            Ok(()) if skip.is_some() => ItemOutcome::Skipped(skip.clone().unwrap_or_default()),
            Ok(()) => ItemOutcome::Updated,
            Err(why) => ItemOutcome::Failed(why),
        };
        if let ItemOutcome::Skipped(why) = outcome {
            skipped(ctx, report, item, why);
            return;
        }
        if outcome == ItemOutcome::Updated {
            let _one_at_a_time = edits_branch_meta(a)
                .then(|| self.arch_check.lock().unwrap_or_else(|p| p.into_inner()));
            super::arch::check_item(a, branch, &item, ctx, report);
        }
        report.results.push(ItemResult {
            item,
            outcome,
            sr: SrOutcome::NotRequested,
        });
    }
}

impl Work for BuildWork<'_, '_> {
    type Job = Job;
    type Out = Report;

    fn keys(job: &Job) -> Vec<String> {
        job.keys.clone()
    }

    fn after(job: &Job) -> Vec<String> {
        job.after.clone()
    }

    fn queued(&self, job: &Job) {
        if let Some(b) = &self.ctx.backend {
            b.register(&job.item);
        }
    }

    fn run(&self, job: Job) -> Report {
        let mut report = Report::default();
        self.build(job, &mut report);
        report
    }

    fn not_run(&self, job: Job, why: String) -> Report {
        self.ctx.warn(format!("{}: {why}", job.item.pkg));
        let mut report = Report::default();
        report.results.push(ItemResult {
            item: job.item,
            outcome: ItemOutcome::Failed(why),
            sr: SrOutcome::NotRequested,
        });
        report
    }

    fn stopping(&self) -> bool {
        ap_build::proc::shutting_down()
    }
}

/// Hand the prepared items over to be built, the dependencies before what
/// needs them. Their results go where the items stand among the results
/// reported so far.
pub(super) fn hand_over(
    ready: &mut Vec<Job>,
    mechanical: &HashSet<String>,
    builds: &mut Builds<'_, '_, '_, '_>,
    report: &Report,
) {
    if ready.is_empty() {
        return;
    }
    let mut batch = std::mem::take(ready);
    batch.sort_by_key(|j| j.item.dependency_of.is_none());
    for job in &mut batch {
        job.mechanical = mechanical.contains(&job.item.pkg);
    }
    builds.submit(batch, report.results.len());
}

/// Add the reports of the builds to `report`: each build's results where its
/// item was handed over, so the results stay in the order the items were found.
pub(super) fn merge_built(report: &mut Report, built: Vec<(usize, Report)>) {
    let mut results = Vec::with_capacity(built.len());
    for (anchor, part) in built {
        results.push((anchor, part.results));
        report.paths.extend(part.paths);
        report.requirements.extend(part.requirements);
        report.arch.extend(part.arch);
        report.maintainer.extend(part.maintainer);
        report.requested.extend(part.requested);
    }
    report.results = interleave(std::mem::take(&mut report.results), results);
}

/// Whether the other-architecture check of `a` edits the branch project's
/// meta: `--arches` adds architectures to it and `--ports` adds repositories.
fn edits_branch_meta(a: &UpdateProjectArgs) -> bool {
    !a.arches.is_empty() || a.ports
}

/// Records an item that is not submitted: it leaves the TUI's offers (so it
/// cannot be submitted from there) and the report says why.
fn skipped(ctx: &Ctx<'_>, report: &mut Report, item: Item, why: String) {
    if let Some(b) = &ctx.backend {
        b.forget(&item);
    }
    report.results.push(ItemResult {
        item,
        outcome: ItemOutcome::Skipped(why),
        sr: SrOutcome::NotRequested,
    });
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
        Ecosystem::Repology { .. } | Ecosystem::Rubygems { .. } | Ecosystem::Cpan { .. } => {
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
        upstream_tag: item.upstream_tag.clone(),
        // A maintenance run (its code streams and backports) keeps its own
        // recipe rules and is not an update of the package.
        update: !a.improve
            && !a.maintenance_source
            && a.backport_job.is_none()
            && a.fix_task.is_none(),
        bug_fix: a.fix_task.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: UpdateProjectArgs,
    }

    fn args(extra: &[&str]) -> UpdateProjectArgs {
        let mut argv = vec!["x", "Base:System"];
        argv.extend_from_slice(extra);
        Wrapper::try_parse_from(argv).unwrap().args
    }

    #[test]
    fn only_a_check_that_edits_the_branch_meta_takes_the_lock() {
        assert!(!edits_branch_meta(&args(&[])));
        assert!(edits_branch_meta(&args(&["--arches", "s390x"])));
        assert!(edits_branch_meta(&args(&["--ports"])));
    }
}
