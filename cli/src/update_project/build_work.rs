//! The build of each prepared item, run by the scheduler on its own thread.

use super::schedule::{Scheduler, Work, interleave};
use super::throttle::Throttle;
use super::tui_row::{FinishWatch, close_row, queue_row, row_name};
use super::*;
use crate::build_cmd::panic_message;
use std::sync::Mutex;

/// A prepared item, as it is handed to the scheduler.
#[derive(Clone)]
pub(super) struct Job {
    pub(super) item: Item,
    /// Its findings are style slips only, committed without a build.
    pub(super) mechanical: bool,
    /// The patches whose headers the tidy rewrites (see `Route::headers`).
    pub(super) headers: Vec<String>,
    /// Its mechanical findings are tidied before the pipeline builds it.
    pub(super) tidy_first: bool,
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
            headers: Vec::new(),
            tidy_first: false,
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
    /// Held by an other-architecture check while it writes the branch
    /// project's meta: two such writes at once would both rewrite it. The
    /// waits of the checks are not held (see `CheckRequest::meta_lock`).
    arch_check: Mutex<()>,
    /// How far the preparation may branch ahead of the builds.
    pub(super) throttle: &'a Throttle,
}

impl<'a, 'c> BuildWork<'a, 'c> {
    pub(super) fn new(
        a: &'a UpdateProjectArgs,
        branch: &'a str,
        resolver: &'a Arc<DepResolver>,
        ctx: &'a Ctx<'c>,
        throttle: &'a Throttle,
    ) -> Self {
        BuildWork {
            a,
            branch,
            resolver,
            ctx,
            arch_check: Mutex::new(()),
            throttle,
        }
    }

    /// The TUI row of `job`'s build, when it has one: a package the TUI
    /// shows and that goes to the pipeline.
    fn row(&self, job: &Job) -> Option<String> {
        self.ctx.tui_ctrl.as_ref()?;
        let a = self.a;
        let pipeline =
            !(a.improve && (job.mechanical || agent_needed(a.no_agent, &job.item.findings)));
        row_name(&job.item).filter(|_| pipeline)
    }

    fn build(&self, job: Job, report: &mut Report, watch: Option<Arc<FinishWatch>>) {
        let (a, branch, ctx, resolver) = (self.a, self.branch, self.ctx, self.resolver);
        // The build closes its row; one that ends without it (it failed
        // first, or reported another name) is closed below, or by `run`.
        let base: Arc<dyn Reporter + Send + Sync> = match &watch {
            Some(w) => w.clone(),
            None => ctx.base_reporter.clone(),
        };
        let build_item = |item: &Item| build_item(a, branch, item, resolver, ctx, base.clone());
        let item = job.item;
        // Why the item is not submitted once its work is done (see `skipped`).
        let mut skip: Option<String> = None;
        let done = if let Some(backport) = &a.backport_job {
            ctx.say(format!(
                "backporting into {} {} in {branch} (the version stays)",
                item.pkg, item.new_version
            ));
            stage_backport(branch, &item, backport, ctx).and_then(|()| build_item(&item))
        } else if let Some(task) = &a.fix_task {
            ctx.say(format!(
                "fixing a bug in {} {} in {branch} (the version stays)",
                item.pkg, item.new_version
            ));
            build_item(&item).and_then(|()| super::fix_landed::check(branch, &item, task, ctx))
        } else if a.improve {
            ctx.say(format!("improving {} in {branch}", item.pkg));
            if job.mechanical {
                match tidy_of(a, branch, &item, ctx, true, &job.headers) {
                    Ok(done) => {
                        skip = super::tidy::tidy_skip(done).map(str::to_string);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else if agent_needed(a.no_agent, &item.findings) {
                skipped(ctx, report, item, "the findings need the agent".to_string());
                return;
            } else if super::unrecognised::unrecognised(&item, &super::unrecognised::probe_archive)
            {
                // The tool's cleanups only: the pipeline would refuse the source.
                match tidy_of(a, branch, &item, ctx, false, &[]) {
                    Ok(_) => {
                        skip = Some(super::unrecognised::SKIPPED.to_string());
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                // The tidy commits the mechanical fixes first, so the build is of the
                // tidied spec. A tidy that fails fails the item, as on the other route.
                let tidied = if job.tidy_first {
                    tidy_of(a, branch, &item, ctx, true, &job.headers).map(|_| ())
                } else {
                    Ok(())
                };
                tidied.and_then(|()| build_item(&item))
            }
        } else {
            ctx.say(format!(
                "updating {} to {} in {branch}",
                item.pkg, item.new_version
            ));
            build_item(&item)
        };
        let outcome = match done {
            Ok(()) if skip.is_some() => ItemOutcome::Skipped(skip.clone().unwrap_or_default()),
            Ok(()) => ItemOutcome::Updated,
            // A build the user's stop ended is not a failure of the package.
            Err(_) if ap_build::proc::interrupted() => ItemOutcome::Interrupted,
            Err(why) => ItemOutcome::Failed(why),
        };
        if let Some(w) = &watch
            && !w.finished()
        {
            close_row(ctx, &w.row, &item, &outcome);
        }
        if let ItemOutcome::Skipped(why) = outcome {
            skipped(ctx, report, item, why);
            return;
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
        // Queued in the TUI when it is handed over, not when its turn comes.
        if let Some(row) = self.row(job) {
            queue_row(self.ctx, &row, &job.item);
        }
    }

    fn run(&self, job: Job) -> Report {
        let mut report = Report::default();
        let item = job.item.clone();
        let watch = self
            .row(&job)
            .map(|row| Arc::new(FinishWatch::new(self.ctx.base_reporter.clone(), row)));
        // A panic in the build is caught here rather than by the scheduler, so
        // the report keeps what the build put in it, and the row is closed only
        // if the build has not closed it.
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.build(job, &mut report, watch.clone())
        }));
        if let Err(p) = built {
            let why = format!("build panicked: {}", panic_message(p.as_ref()));
            self.ctx.warn(format!("{}: {why}", item.pkg));
            let outcome = ItemOutcome::Failed(why);
            if let Some(w) = watch.as_ref().filter(|w| !w.finished()) {
                close_row(self.ctx, &w.row, &item, &outcome);
            }
            report.results.push(ItemResult {
                item,
                outcome,
                sr: SrOutcome::NotRequested,
            });
        }
        // The other-architecture waits of the run end `--arch-wait-minutes`
        // after the last build ends (see `ArchDeadline`).
        self.ctx.arch_deadline.build_ended(Instant::now());
        report
    }

    /// A job the user's stop keeps from starting: neither a failure nor a
    /// warning. The run's summary counts it (see `report::interrupt_summary`).
    fn not_started(&self, job: Job) -> Report {
        let outcome = ItemOutcome::NotStarted;
        if let Some(row) = self.row(&job) {
            close_row(self.ctx, &row, &job.item, &outcome);
        }
        let mut report = Report::default();
        report.results.push(ItemResult {
            item: job.item,
            outcome,
            sr: SrOutcome::NotRequested,
        });
        report
    }

    fn not_run(&self, job: Job, why: String) -> Report {
        self.ctx.warn(format!("{}: {why}", job.item.pkg));
        let outcome = ItemOutcome::Failed(why);
        if let Some(row) = self.row(&job) {
            close_row(self.ctx, &row, &job.item, &outcome);
        }
        let mut report = Report::default();
        report.results.push(ItemResult {
            item: job.item,
            outcome,
            sr: SrOutcome::NotRequested,
        });
        report
    }

    /// The other-architecture check of each item the build updated. It waits
    /// for OBS on the other arches, so it runs after the build's slot is free.
    fn post(&self, mut report: Report) -> Report {
        let updated: Vec<Item> = report
            .results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::Updated)
            .map(|r| r.item.clone())
            .collect();
        for item in updated {
            // A check that panics is a warning, as a check that cannot run is.
            let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::arch::check_item(
                    self.a,
                    self.branch,
                    &item,
                    self.ctx,
                    &self.arch_check,
                    &mut report,
                );
            }));
            if let Err(p) = checked {
                self.ctx.warn(format!(
                    "arch check of {} panicked: {}",
                    item.pkg,
                    panic_message(p.as_ref())
                ));
            }
        }
        report
    }

    fn stopping(&self) -> bool {
        ap_build::proc::shutting_down()
    }

    fn run_ended(&self) {
        self.throttle.run_ended();
    }
}

/// Hand the prepared items over to be built, the dependencies before what
/// needs them. Their results go where the items stand among the results
/// reported so far. The throttle counts them from now until their runs end,
/// and the next unit starts from nothing, also when this one has no items.
pub(super) fn hand_over(
    ready: &mut Vec<Job>,
    routes: &Routes,
    builds: &mut Builds<'_, '_, '_, '_>,
    report: &Report,
) {
    builds.work().throttle.handed_over(ready.len());
    if ready.is_empty() {
        return;
    }
    let mut batch = std::mem::take(ready);
    batch.sort_by_key(|j| j.item.dependency_of.is_none());
    for job in &mut batch {
        job.mechanical = routes.mechanical(&job.item.pkg);
        job.headers = routes.headers(&job.item.pkg).to_vec();
        job.tidy_first = routes.tidy_first(&job.item.pkg);
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

/// Whether a `lint` build reviews `item` as its own work only (see
/// `BranchBuild::scoped`): the package carries unpublished work, and `--fix`
/// is not given. The carry is read as the audit reads it.
fn lint_scoped(a: &UpdateProjectArgs, branch: &str, item: &Item, ctx: &Ctx<'_>) -> bool {
    a.improve
        && !a.fix_inherited
        && super::leftovers::carry(
            &a.source,
            branch,
            &item.pkg,
            a.in_place,
            super::existing_lint::applies(a),
            Some(&item.ecosystem),
            Emit::run(ctx.reporter.as_ref()),
        )
        .is_some()
}

/// The tidy of `item` in `branch` (see `tidy_item`), with the ldconfig macro of
/// the branch's target. `style` says whether its style fixes are made too, and
/// `headers` which patches' headers are rewritten.
fn tidy_of(
    a: &UpdateProjectArgs,
    branch: &str,
    item: &Item,
    ctx: &Ctx<'_>,
    style: bool,
    headers: &[String],
) -> Result<super::tidy::TidyDone, String> {
    let target = osc::choose_repository(branch, a.distro.as_deref(), &a.arch).0;
    let ldconfig = ap_build::spec_tidy::hygiene::ldconfig_macro_defined(&target);
    super::tidy::tidy_item(branch, item, ctx, ldconfig, a.no_agent, style, headers)
}

/// Build `item` in the branch project with the ordinary pipeline.
fn build_item(
    a: &UpdateProjectArgs,
    branch: &str,
    item: &Item,
    resolver: &Arc<DepResolver>,
    ctx: &Ctx<'_>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
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
        lint: a.improve,
        scoped: lint_scoped(a, branch, item, ctx),
        from_version: Some(item.from_version().to_string()),
        upstream_tag: item.upstream_tag.clone(),
        // A maintenance run (its code streams and backports) keeps its own
        // recipe rules and is not an update of the package.
        update: !a.improve
            && !a.maintenance_source
            && a.backport_job.is_none()
            && a.fix_task.is_none(),
        bug_fix: a.fix_task.clone(),
        scheme: super::existing::scheme_of_ecosystem(&item.ecosystem),
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
            base_reporter,
            ctx.tui_ctrl.clone(),
            Some(&build),
            None,
            None,
        )
    })
    .map_err(|e| format!("{e:#}"))
}
