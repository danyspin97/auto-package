//! `update`: update the packages of an OBS project that have a newer
//! upstream release, in a branch project, and optionally submit them back.
//!
//! The source project is only ever read (listing, spec fetches for the plan).
//! Packages are branched into the branch project and built there by the
//! ordinary pipeline, exactly as `update` builds in its own project, so the
//! fix loop, notes, budget and locking behave the same.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ap_build::depreach::{self, DepResolver, DepState, DryRunObs, LiveObs};
use ap_build::osc::{self, BranchResult, PathAddition};
use ap_build::proc::Emit;
use ap_build::quality;
use ap_build::update::{self, Ecosystem};
use ap_build::update_project::{self as up, Item, ItemOutcome, ItemResult, SrOutcome};
use ap_core::event::{Event, RunOutcome};
use ap_core::run::RunDir;
use ap_core::sink::{FanoutReporter, HumanSink, JsonlSink, LogLevel, MemorySink, Reporter};
use color_eyre::eyre::eyre;

use super::{
    ProjectLock, Semaphore, Sweep, catch_panic, cmd_build, install_interrupt_handler,
    make_semaphore, print_run_summary, reject_symlink_project_dir, repo_package_name, tui,
};

/// How many packages `--update-deps` may pull in, against a dependency chain
/// that keeps growing.
const MAX_DEPENDENCY_ITEMS: usize = 25;

#[derive(clap::Args)]
pub struct UpdateProjectArgs {
    /// OBS project to browse (e.g. Base:System): for packages with a newer
    /// upstream release, or with `lint` for quality problems. It is
    /// only read, unless `--in-place` is given.
    pub source: String,

    /// Project to branch the packages into and build them in. Defaults to
    /// `home:<osc user>:branches:<SOURCE>`, what `osc branch` uses. Created by
    /// the first branch when it does not exist.
    #[arg(long)]
    pub branch_project: Option<String>,

    /// Build the new versions (`lint`: commit the fixes) straight in SOURCE
    /// instead of in a branch project. Nothing is submitted, since nothing is
    /// branched; use it on a project you own.
    #[arg(long, default_value_t = false)]
    pub in_place: bool,

    /// Set by `lint`: take the packages from the quality audit of
    /// SOURCE instead of from the upstream release sweep.
    #[arg(skip)]
    pub improve: bool,

    /// Only update these packages (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,

    /// Leave these packages out (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,

    /// Create a submit request to the package's origin project for every
    /// package that built successfully in the branch project. Asks for
    /// confirmation first; see `--yes`.
    #[arg(long, default_value_t = false)]
    pub submit: bool,

    /// Create the submit requests without asking.
    #[arg(short, long, default_value_t = false)]
    pub yes: bool,

    /// Print the plan, the branch project, the build paths that would be added
    /// and the requests that would be created, and stop. Changes nothing on
    /// OBS.
    #[arg(long, alias = "check", default_value_t = false)]
    pub dry_run: bool,

    /// Also branch and update a dependency that lives in another project and
    /// has a newer upstream release itself; its request goes to its own
    /// origin project. Without this, such a dependency is only made reachable
    /// through a build path of the branch project, and never touched.
    #[arg(long, default_value_t = false)]
    pub update_deps: bool,

    /// Branch from a package's devel project, when it has one, instead of
    /// from the project named by SOURCE; its submit request then goes to the
    /// devel project. Without this the package is branched from SOURCE as
    /// asked, which skips the devel project's own review, and a warning says
    /// so for each package that has one.
    #[arg(long, default_value_t = false)]
    pub follow_devel: bool,

    #[arg(long, default_value = "ap-work")]
    pub work_dir: PathBuf,

    /// Directory for per-package notes, shared across projects. Kept outside
    /// work_dir so they survive cleanup.
    #[arg(long = "notes", alias = "notes-dir", default_value = "notes/packages")]
    pub notes_dir: PathBuf,

    /// Repository to build for (default: the branch project's own, Tumbleweed
    /// first; it is the one the source project builds for).
    #[arg(long)]
    pub distro: Option<String>,

    #[arg(long, default_value = "x86_64")]
    pub arch: String,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    pub log: String,

    /// Enable interactive TUI mode for the per-package builds. Cannot be
    /// combined with `--submit` unless `--yes` is also given: the TUI owns the
    /// terminal, so there is nowhere to ask.
    #[arg(long, default_value_t = false)]
    pub tui: bool,

    /// Maximum agent fix attempts per package.
    #[arg(long, default_value_t = 10)]
    pub max_attempts: usize,

    /// USD budget cap for agent calls (per package build).
    #[arg(long, default_value_t = 10.0)]
    pub budget_usd: f64,

    /// Build only; never call the model.
    #[arg(long, default_value_t = false)]
    pub no_agent: bool,

    /// Ask PyPI and the forges for every package's newest version, instead of
    /// reusing an answer from the last hour (kept in the work directory).
    #[arg(long, default_value_t = false)]
    pub refresh: bool,

    /// Resolve dependencies without the sandbox: pip runs on the host and
    /// may build sdists, running their build backends as you. Without
    /// this, pip runs in bubblewrap, or resolves wheels only when
    /// bubblewrap is unusable. Same as AUTO_PACKAGE_UNSAFE_RESOLVE=1.
    #[arg(long, default_value_t = false)]
    pub unsafe_resolve: bool,

    /// Maximum number of packages to build concurrently (0 = unlimited). Each
    /// build still runs its own dependency closure, so this bounds the
    /// top-level loop, not every spawned sub-build.
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,

    /// Wait for this many GiB free on the work directory's disk before a
    /// package starts its heavy stages (0 = never wait).
    #[arg(long, default_value_t = 3)]
    pub min_free_gb: u64,

    /// How the NVIDIA CUDA libraries are packaged: `wheel` (the default) or
    /// `redist` (see `build --nvidia`). Remembered in the work directory.
    #[arg(long, value_enum, value_name = "MODE")]
    pub nvidia: Option<crate::NvidiaMode>,

    /// Same as `--nvidia redist`.
    #[arg(long, default_value_t = false, conflicts_with = "nvidia")]
    pub nvidia_redist: bool,

    /// Regenerate each package's spec from scratch instead of carrying the
    /// existing recipe forward (see `update --regen`).
    #[arg(long, default_value_t = false)]
    pub regen: bool,
}

/// What `update` changes about how one package is built.
pub struct BranchBuild {
    /// The package's name in the branch project, for a repository package whose
    /// name the pipeline would otherwise guess.
    pub rpm_name: Option<String>,
    /// Looks a missing dependency up on OBS before the pipeline builds it.
    pub resolver: Option<Arc<DepResolver>>,
    /// `lint`: what the quality checks found, handed to the agent as
    /// the problem to fix in the existing recipe.
    pub improve: Option<String>,
}

/// Everything a run produced, kept outside the `Result` so the summary is
/// printed even when the run fails part-way.
#[derive(Default)]
struct Report {
    results: Vec<ItemResult>,
    paths: Vec<PathAddition>,
}

/// What the build needs from `run`'s setup.
struct Ctx<'a> {
    work_dir: PathBuf,
    state_root: PathBuf,
    rpms_dir: PathBuf,
    source_urls: Arc<ap_build::sources::SourceMap>,
    python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    run_dir: &'a RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
    tui_ctrl: Option<tui::TuiController>,
    sem: Option<Semaphore>,
}

impl Ctx<'_> {
    fn say(&self, message: impl Into<String>) {
        self.reporter.report(&Event::Progress {
            step: None,
            message: message.into(),
        });
    }

    fn warn(&self, message: impl Into<String>) {
        self.reporter.report(&Event::Warning {
            step: None,
            message: message.into(),
        });
    }
}

pub fn run(a: UpdateProjectArgs, summary: &mut Option<String>) -> color_eyre::Result<()> {
    if a.in_place && (a.submit || a.branch_project.is_some()) {
        return Err(eyre!(
            "--in-place changes {} itself: there is no branch to name and no request to submit",
            a.source
        ));
    }
    if a.in_place && !a.improve && (a.update_deps || a.follow_devel) {
        return Err(eyre!(
            "--update-deps and --follow-devel branch other projects: they cannot be combined with --in-place"
        ));
    }
    if a.improve && a.update_deps {
        return Err(eyre!("--update-deps is only for update"));
    }
    ap_ecosystem::pip::set_unsafe_resolve(a.unsafe_resolve);
    std::fs::create_dir_all(&a.work_dir)?;
    ap_build::upstream_cache::set_dir(std::path::absolute(&a.work_dir)?);
    ap_build::upstream_cache::set_refresh(a.refresh);
    if a.in_place && !a.improve {
        return super::update_in_place(a, summary);
    }
    let branch_project = match &a.branch_project {
        _ if a.in_place => a.source.clone(),
        Some(b) => b.clone(),
        None => {
            let user = osc::current_user(Emit::quiet()).map_err(|e| eyre!("{e}"))?;
            osc::default_branch_project(&user, &a.source)
        }
    };
    for (what, name) in [("source", &a.source), ("branch", &branch_project)] {
        if !osc::is_valid_obs_name(name) {
            return Err(eyre!("{name:?} is not a valid {what} project name"));
        }
    }
    if branch_project == a.source && !a.in_place {
        return Err(eyre!(
            "the branch project must not be the source project: {} is never written to",
            a.source
        ));
    }
    if a.tui && a.submit && !a.yes && !a.dry_run {
        return Err(eyre!(
            "--submit asks for confirmation, which the TUI cannot do: pass --yes, or drop --tui"
        ));
    }

    ap_build::discovery::set_agent_enabled(!a.no_agent);
    if !a.no_agent {
        ap_build::agent::ensure_logged_in().map_err(|e| eyre!(e))?;
    }
    ap_build::package_notes::set_notes_dir(std::path::absolute(&a.notes_dir)?);
    let level: LogLevel = a
        .log
        .parse()
        .map_err(|e: String| color_eyre::eyre::eyre!(e))?;

    // The pipeline's state is per project, so the branch project is the one
    // that names the work directory (same layout as `update`).
    std::fs::create_dir_all(&a.work_dir)?;
    let project_dir = a.work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(&branch_project),
        ap_core::run::sanitize_name(&a.arch),
    ));
    reject_symlink_project_dir(&project_dir)?;
    std::fs::create_dir_all(&project_dir)?;
    let _project_lock = ProjectLock::acquire(&project_dir)?;
    let work_dir = project_dir.canonicalize()?;
    crate::init_disk_guard(
        &work_dir,
        a.min_free_gb,
        &branch_project,
        &osc::choose_repository(&branch_project, a.distro.as_deref(), &a.arch).0,
        &a.arch,
    );
    let nvidia_choice = a
        .nvidia
        .or(a.nvidia_redist.then_some(crate::NvidiaMode::Redist));
    let nvidia_mode = crate::apply_nvidia_mode(&work_dir, nvidia_choice, false)?;
    crate::write_nvidia_mode(&work_dir, nvidia_mode, nvidia_choice.is_some())?;
    let rpms_dir = work_dir.join("rpms");
    std::fs::create_dir_all(&rpms_dir)?;
    let state_root = work_dir.join("state");
    std::fs::create_dir_all(&state_root)?;

    let source_urls =
        Arc::new(ap_build::sources::SourceMap::load(&work_dir).map_err(|e| eyre!(e))?);
    let python_flavors =
        Arc::new(ap_build::flavors::PythonFlavors::load(&work_dir).map_err(|e| eyre!(e))?);

    let run_dir = RunDir::allocate(&work_dir)?;
    let memory = Arc::new(MemorySink::new());
    // Declared before `tui_ctrl` so it drops last (see `Commands::Build`).
    let mut _terminal_restore = a.tui.then(tui::TerminalGuard::new);
    let tui_ctrl = crate::start_tui(
        a.tui,
        memory.clone(),
        tui::RunScope {
            project: branch_project.clone(),
            targets: a.only.clone(),
        },
        false,
    );
    if tui_ctrl.is_none() {
        _terminal_restore = None;
    }
    ap_core::sink::set_quiet(tui_ctrl.is_some());

    let mut sinks: Vec<Box<dyn Reporter>> = Vec::new();
    if tui_ctrl.is_none() {
        sinks.push(Box::new(HumanSink::new(level)));
    }
    sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
    sinks.push(Box::new(memory.clone()));
    let base_reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(FanoutReporter::new(sinks));
    let reporter: Arc<dyn Reporter + Send + Sync> = match &tui_ctrl {
        Some(t) => t.tap(base_reporter.clone(), None),
        None => base_reporter.clone(),
    };
    install_interrupt_handler(tui_ctrl.is_some(), memory.clone())?;

    reporter.report(&Event::RunStarted {
        run_id: run_dir.run_id.clone(),
        argv: std::env::args().collect(),
        work_dir: work_dir.clone(),
    });

    let ctx = Ctx {
        work_dir,
        state_root,
        rpms_dir,
        source_urls,
        python_flavors,
        run_dir: &run_dir,
        reporter: reporter.clone(),
        base_reporter,
        tui_ctrl,
        sem: make_semaphore(a.jobs),
    };

    let start = Instant::now();
    let mut report = Report::default();
    let result = cmd_update_project(&a, &branch_project, &ctx, &mut report);
    if let Some(t) = &ctx.tui_ctrl {
        t.work_finished();
    }
    let failed = up::any_failed(&report.results);

    reporter.report(&Event::RunFinished {
        outcome: if result.is_ok() && !failed {
            RunOutcome::Success
        } else {
            RunOutcome::Failed
        },
        elapsed: start.elapsed(),
        usage: memory.usage(),
    });
    if !a.dry_run {
        *summary = Some(format!(
            "{}{}",
            print_run_summary(
                &memory,
                &branch_project,
                &osc::choose_repository(&branch_project, a.distro.as_deref(), &a.arch).0,
                &a.arch,
                &ctx.state_root
            ),
            up::summary_text(&branch_project, &report.results, &report.paths, a.submit)
        ));
    }

    let failures = if failed {
        let n = report
            .results
            .iter()
            .filter(|r| matches!(r.outcome, ItemOutcome::Failed(_)))
            .count();
        vec![format!("{n} package(s) failed to update")]
    } else {
        Vec::new()
    };
    match &ctx.tui_ctrl {
        Some(t) => {
            t.wait_closed();
            crate::tui_run_exit(t.all_settled(), t.failure(), result, failures)
        }
        None => {
            result?;
            if failures.is_empty() {
                Ok(())
            } else {
                Err(eyre!(failures.join("; ")))
            }
        }
    }
}

/// The packages whose names `only` and `exclude` allow, as the listing spells
/// them.
pub fn wanted_by(only: &[String], exclude: &[String], listed: &str) -> bool {
    let pkg = up::branch_name(listed);
    (only.is_empty() || up::name_listed(only, pkg)) && !up::name_listed(exclude, pkg)
}

fn cmd_update_project(
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
    if let Some(kind) = &source_info.kind {
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

    // `update` sweeps the source for newer releases and `lint` audits it, each
    // on its own thread. A package that has a release, or findings, is branched
    // and built as it turns up, while the thread goes on with the others.
    let mut mechanical: HashSet<String> = HashSet::new();
    let (mut sweep, mut audit) = (None, None);
    if a.improve {
        audit = Some(Audit::start(a, ctx.reporter.clone()));
    } else {
        sweep = Some(Sweep::start(
            &a.source,
            &a.only,
            &a.exclude,
            ctx.reporter.clone(),
        ));
    }
    // The next package to work on, in the order it was found, and whether its
    // findings are only style slips, which are committed without a build.
    let incoming = || -> Option<(Item, bool)> {
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

    // A dry run reads the branch project as branching would leave it.
    let resolver = {
        let live = LiveObs {
            reporter: ctx.reporter.clone(),
        };
        let resolver_for = |obs: Box<dyn depreach::Obs>| {
            DepResolver::new(obs, ctx.reporter.clone(), branch, &a.arch)
        };
        if a.dry_run && !a.in_place {
            resolver_for(Box::new(DryRunObs {
                inner: live,
                branch_project: branch.to_string(),
                branch_meta: depreach::synthetic_branch_meta(branch, &a.source, &source_info),
            }))
        } else {
            resolver_for(Box::new(live))
        }
    };
    let resolver = Arc::new(resolver);

    // Branch, then make each package's BuildRequires reachable. With
    // `--update-deps` a dependency from another project that has a newer
    // release is queued behind the package that needs it.
    let mut queue: VecDeque<Item> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut ready: Vec<Item> = Vec::new();
    let mut admitted = 0;
    let mut dependency_count = 0;
    let mut branch_checked = false;
    loop {
        let mut item = match queue.pop_front() {
            Some(item) => item,
            None => {
                // The package and what it needs are all prepared: build them
                // now, then wait for the sweep to find the next.
                if !a.dry_run {
                    build_ready(a, branch, &mut ready, &mechanical, &resolver, ctx, report);
                }
                let Some((item, style_only)) = incoming() else {
                    break;
                };
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
        if a.in_place {
            // The package is already where it will be changed, and its
            // BuildRequires resolve: it builds there today.
            ready.push(item);
            continue;
        }
        if !a.dry_run {
            match branch_item(branch, &mut item, a.follow_devel, ctx) {
                Ok(()) => {}
                Err(why) => {
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
                update::spec_for_package(&item.origin, &item.pkg, emit)
            } else {
                update::planned_spec(&item.origin, &item.pkg, emit)
            }
        };
        let spec = if a.dry_run {
            origin_spec(&item)
        } else {
            update::spec_for_package(branch, &item.pkg, emit).or_else(|| origin_spec(&item))
        };
        match spec {
            Some(spec) => {
                let (adds, verdicts) =
                    match reach_dependencies(a, &item, &spec, &resolver, ctx, report) {
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
                    for dep in dependency_items(a, branch, &item, &verdicts, &adds, ctx, emit) {
                        if dependency_count >= MAX_DEPENDENCY_ITEMS {
                            ctx.warn(format!(
                                "--update-deps: not updating {} too, the limit is {MAX_DEPENDENCY_ITEMS}",
                                dep.pkg
                            ));
                        } else if visited.insert(visit_key(&dep)) {
                            dependency_count += 1;
                            ctx.say(format!(
                                "{} needs {} from {}, which has a newer release ({} -> {})",
                                item.pkg, dep.pkg, dep.origin, dep.old_version, dep.new_version
                            ));
                            queue.push_back(dep);
                        }
                    }
                }
            }
            None => ctx.warn(format!(
                "could not read the spec of {}, so its BuildRequires are not checked",
                item.pkg
            )),
        }
        ready.push(item);
    }

    if let Some(sweep) = sweep.take() {
        let plan = sweep.finish().map_err(|e| eyre!("{e}"))?;
        if plan.total() == 0 {
            ctx.say(format!("no matching packages in {}", a.source));
            return Ok(());
        }
        if plan.n_update == 0 {
            ctx.say("nothing to update");
            return Ok(());
        }
    }
    if let Some(audit) = audit.take() {
        let end = audit.finish().map_err(|e| eyre!("{e}"))?;
        if end.packages == 0 {
            ctx.say(format!("no matching packages in {}", a.source));
            return Ok(());
        }
        if end.items == 0 {
            ctx.say("nothing to improve");
            return Ok(());
        }
    }
    if admitted == 0 {
        return Ok(());
    }

    if a.dry_run && a.improve {
        ctx.say(format!(
            "dry run: nothing is branched, built, edited or submitted; {} stays untouched",
            a.source
        ));
        for i in &ready {
            let how = if mechanical.contains(&i.pkg) {
                "style fixes committed as they are"
            } else if a.no_agent {
                "needs the agent (--no-agent is set), would be skipped"
            } else {
                "rebuilt by the pipeline, the agent fixing the findings"
            };
            ctx.say(format!("  {}: {how}", i.pkg));
        }
        if a.in_place {
            ctx.say(format!(
                "the fixes would be committed into {} itself",
                a.source
            ));
        } else if a.submit {
            ctx.say(format!(
                "each package that builds in {branch} would get a submit request to {}",
                a.source
            ));
        } else {
            ctx.say(format!(
                "they would be branched into {branch}; submit requests: none (--submit not given)"
            ));
        }
        report.paths.clear();
        return Ok(());
    }
    if a.dry_run {
        let planned: Vec<PathAddition> = report.paths.clone();
        for line in up::dry_run_lines(&up::DryRun {
            source: &a.source,
            branch_project: branch,
            branch_exists: branch_meta.is_some(),
            items: &ready,
            paths: &planned,
            submit: a.submit,
        }) {
            ctx.say(line);
        }
        report.paths.clear();
        return Ok(());
    }

    build_ready(a, branch, &mut ready, &mechanical, &resolver, ctx, report);

    if a.submit {
        submit_requests(a, branch, &mut report.results, ctx);
    }
    Ok(())
}

/// The audit of `lint`: what is wrong with each package of the source, printed
/// as a report once it is done. It runs on its own thread, and every package
/// with findings that the pipeline can rebuild arrives on `items` as soon as it
/// is audited, with whether its findings are all style slips, which are
/// committed without a build.
struct Audit {
    items: std::sync::mpsc::Receiver<(Item, bool)>,
    handle: std::thread::JoinHandle<Result<AuditEnd, String>>,
}

/// How an audit ended.
struct AuditEnd {
    /// The packages audited.
    packages: usize,
    /// The packages handed on to be improved.
    items: usize,
}

impl Audit {
    fn start(a: &UpdateProjectArgs, reporter: Arc<dyn Reporter + Send + Sync>) -> Self {
        let (tx, items) = std::sync::mpsc::channel();
        let (source, distro, arch) = (a.source.clone(), a.distro.clone(), a.arch.clone());
        let (only, exclude) = (a.only.clone(), a.exclude.clone());
        let handle = std::thread::spawn(move || {
            let say = |message: String| {
                reporter.report(&Event::Progress {
                    step: None,
                    message,
                })
            };
            let warn = |message: String| {
                reporter.report(&Event::Warning {
                    step: None,
                    message,
                })
            };
            let emit = Emit::run(reporter.as_ref());
            let distro = osc::choose_repository(&source, distro.as_deref(), &arch).0;
            let statuses = osc::get_project_build_statuses(&source, &distro, &arch, emit)
                .map_err(|e| {
                    warn(format!(
                        "could not read the build results of {source}, so broken builds and rpmlint are not checked: {e}"
                    ))
                })
                .unwrap_or_default();

            let mut audits: Vec<quality::PackageAudit> = Vec::new();
            let mut sent = 0;
            let mut seen: HashSet<String> = HashSet::new();
            update::plan_each(
                &source,
                &|n| wanted_by(&only, &exclude, n),
                emit,
                &mut |c| {
                    let pkg = up::branch_name(&c.rpm_name);
                    if !seen.insert(pkg.to_string()) {
                        return true;
                    }
                    let Some(spec) = update::planned_spec(&source, pkg, emit) else {
                        warn(format!(
                            "could not read the spec of {pkg}, so it is not checked"
                        ));
                        return true;
                    };
                    let mut findings = quality::audit_spec(&spec);
                    let status = statuses.get(&c.rpm_name).or_else(|| statuses.get(pkg));
                    if let Some(status) = status {
                        findings.extend(quality::audit_status(status));
                        if status.is_success()
                            && let Some(log) =
                                osc::fetch_build_log_tail(&source, pkg, &distro, &arch)
                        {
                            findings.extend(quality::rpmlint_findings(&log));
                        }
                    }
                    findings.sort_by_key(|f| f.kind);

                    let (ecosystem, version) = match &c.decision {
                        update::Decision::Update {
                            ecosystem,
                            old_version,
                            ..
                        } => (Some(ecosystem.clone()), old_version.clone()),
                        update::Decision::UpToDate { ecosystem, version } => {
                            (Some(ecosystem.clone()), version.clone())
                        }
                        update::Decision::Skipped(_) => (None, String::new()),
                    };
                    let mut go_on = true;
                    if !findings.is_empty() {
                        match ecosystem {
                            Some(ecosystem) => {
                                let style_only = !quality::needs_pipeline(&findings);
                                let item = Item {
                                    pkg: pkg.to_string(),
                                    origin: source.clone(),
                                    ecosystem,
                                    old_version: version.clone(),
                                    new_version: version,
                                    dependency_of: None,
                                    findings: findings
                                        .iter()
                                        .map(|f| format!("{}: {}", f.kind.label(), f.detail))
                                        .collect(),
                                };
                                // A closed channel means the run gave up.
                                go_on = tx.send((item, style_only)).is_ok();
                                sent += 1;
                            }
                            None => say(format!(
                                "{pkg}: its source is not one auto-package knows, so its findings are only reported"
                            )),
                        }
                    }
                    audits.push(quality::PackageAudit {
                        pkg: pkg.to_string(),
                        findings,
                    });
                    go_on
                },
            )?;
            for line in quality::report_lines(&audits) {
                say(line);
            }
            Ok(AuditEnd {
                packages: audits.len(),
                items: sent,
            })
        });
        Self { items, handle }
    }

    /// How the audit ended, once `items` has run dry.
    fn finish(self) -> Result<AuditEnd, String> {
        drop(self.items);
        self.handle
            .join()
            .unwrap_or_else(|_| Err("the audit of the project panicked".to_string()))
    }
}

/// The problem the agent is asked to fix for an `lint` item.
fn improve_problem(item: &Item) -> Option<String> {
    if item.findings.is_empty() {
        return None;
    }
    Some(format!(
        "{} {} builds, but the quality checks of the project found these problems. Fix each \
         one in the existing recipe where it is a real defect, and keep the version and every \
         other decision. An rpmlint warning that is a false positive gets a justified \
         `rpmlintrc` filter (convention 39), not a silent skip. Do not change `%check` into a \
         disabled one to get a clean run.\n\nFindings:\n- {}",
        item.pkg,
        item.new_version,
        item.findings.join("\n- ")
    ))
}

/// Commit the style fixes of `quality::tidy` for `item` into `branch`, without
/// a build of our own: OBS rebuilds the commit.
fn tidy_item(branch: &str, item: &Item, ctx: &Ctx<'_>) -> Result<(), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let dir = osc::checkout(branch, &item.pkg, &ctx.work_dir.join("improve"), emit)
        .map_err(|e| e.to_string())?;
    let files: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let spec_name = update::pick_spec_filename(&item.pkg, &files)
        .ok_or_else(|| "the package has no spec".to_string())?;
    let spec_path = dir.join(&spec_name);
    let before = std::fs::read_to_string(&spec_path).map_err(|e| e.to_string())?;
    let after = quality::tidy(&before);
    if after == before {
        ctx.say(format!("{}: already tidy in {branch}", item.pkg));
        return Ok(());
    }
    // Kept so that a failure after the write leaves the reused checkout as it was.
    let changes = format!("{}.changes", item.pkg);
    let changes_path = dir.join(&changes);
    let changes_before = std::fs::read(&changes_path).ok();
    ap_core::fsx::write_atomic(&spec_path, after.as_bytes()).map_err(|e| e.to_string())?;
    let committed = (|| -> Result<(), String> {
        osc::add_changes_entry(
            &dir,
            "Tidy the spec: section order, noarch, %{version}",
            emit,
        )
        .map_err(|e| e.to_string())?;
        let mut commit_files = vec![spec_name.clone()];
        if changes_path.exists() {
            commit_files.push(changes.clone());
        }
        osc::commit(
            &dir,
            "Tidy the spec (auto-package lint)",
            &commit_files,
            emit,
        )
        .map_err(|e| e.to_string())
    })();
    if let Err(why) = committed {
        let restored_spec = ap_core::fsx::write_atomic(&spec_path, before.as_bytes());
        let restored_changes = match changes_before {
            Some(bytes) => ap_core::fsx::write_atomic(&changes_path, bytes),
            None => match std::fs::remove_file(&changes_path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        if let Err(e) = restored_spec.and(restored_changes) {
            return Err(format!("{why}; could not restore the checkout of {branch}: {e}"));
        }
        return Err(why);
    }
    ctx.say(format!(
        "{}: committed the style fixes to {branch}",
        item.pkg
    ));
    Ok(())
}

/// The newest `.changes` entry of a package in the branch project.
fn newest_changes(branch: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    let files = osc::list_package_files(branch, pkg, emit)?;
    let own = format!("{pkg}.changes");
    let name = files
        .iter()
        .find(|f| **f == own)
        .or_else(|| files.iter().find(|f| f.ends_with(".changes")))?;
    up::latest_changes_entry(&osc::cat_file(branch, pkg, name, emit)?)
}

/// Ask a yes/no question on the terminal. Without a terminal there is nobody
/// to ask, which counts as no.
fn confirm(question: &str, ctx: &Ctx<'_>) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        ctx.warn("cannot ask for confirmation without a terminal; pass --yes to submit unasked");
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Create a submit request, to its origin project, for each package that
/// built at the new version in the branch project.
///
/// Conservative at every step: a package whose build or version cannot be
/// confirmed gets no request; one with an open request from the branch
/// project already gets none either (and a request list that cannot be read
/// counts as unknown, not as empty); and nothing is created before the list
/// is shown and confirmed, unless `--yes`.
fn submit_requests(a: &UpdateProjectArgs, branch: &str, results: &mut [ItemResult], ctx: &Ctx<'_>) {
    let emit = Emit::run(ctx.reporter.as_ref());
    let mut pending: Vec<(usize, osc::SubmitRequest)> = Vec::new();
    for (idx, r) in results.iter_mut().enumerate() {
        let updated = r.outcome == ItemOutcome::Updated;
        let build = updated
            .then(|| {
                osc::get_overall_build_status(
                    branch,
                    &r.item.pkg,
                    &crate::choose_distro(a.distro.as_deref(), branch, &a.arch),
                    &a.arch,
                    emit,
                )
                // Name the repository/arch that blocks, not just its status.
                .map(|(status, at)| match (&status, at) {
                    (osc::BuildStatus::Succeeded, _) | (_, None) => status,
                    (_, Some(at)) => osc::BuildStatus::Unknown(format!("{status} on {at}")),
                })
                .map_err(|e| {
                    ctx.warn(format!(
                        "could not read the build status of {}: {e}",
                        r.item.pkg
                    ))
                })
                .ok()
            })
            .flatten();
        let spec_version = updated
            .then(|| update::spec_for_package(branch, &r.item.pkg, emit))
            .flatten()
            .and_then(|spec| ap_build::spec::parse_header(&spec))
            .map(|h| h.version);
        if let Err(why) =
            up::sr_eligibility(&r.item, &r.outcome, build.as_ref(), spec_version.as_deref())
        {
            ctx.say(format!("no request for {}: {why}", r.item.pkg));
            r.sr = SrOutcome::NotEligible(why);
            continue;
        }
        match osc::open_submit_requests(branch, &r.item.pkg, &r.item.origin, emit) {
            Ok(open) => {
                if let Some(first) = open.first() {
                    ctx.say(format!(
                        "no request for {}: request {} from {branch} to {} is already open",
                        r.item.pkg, first.id, r.item.origin
                    ));
                    r.sr = SrOutcome::Existing(first.id.clone());
                    continue;
                }
            }
            Err(e) => {
                ctx.warn(format!(
                    "no request for {}: could not check for an open one: {e}",
                    r.item.pkg
                ));
                r.sr = SrOutcome::Failed(format!("could not check for an open request: {e}"));
                continue;
            }
        }
        pending.push((
            idx,
            osc::SubmitRequest {
                source_project: branch.to_string(),
                source_package: r.item.pkg.clone(),
                target_project: r.item.origin.clone(),
                target_package: r.item.pkg.clone(),
                description: up::sr_description(
                    &r.item,
                    newest_changes(branch, &r.item.pkg, emit).as_deref(),
                ),
            },
        ));
    }
    if pending.is_empty() {
        return;
    }

    let items: Vec<&Item> = pending.iter().map(|(i, _)| &results[*i].item).collect();
    ctx.say(format!("submit requests to create ({}):", items.len()));
    for line in up::sr_confirmation_lines(branch, &items) {
        ctx.say(format!("  {line}"));
    }
    if !a.yes && !confirm(&format!("Create {} submit request(s)?", pending.len()), ctx) {
        for (idx, _) in &pending {
            results[*idx].sr = SrOutcome::Declined;
        }
        return;
    }

    for (idx, sr) in pending {
        results[idx].sr = match osc::create_submit_request(&sr, emit) {
            Ok(id) => {
                ctx.say(format!(
                    "created request {id}: {}/{} -> {}/{}",
                    sr.source_project, sr.source_package, sr.target_project, sr.target_package
                ));
                SrOutcome::Created(id)
            }
            Err(e) => {
                ctx.warn(format!(
                    "could not create the request for {}: {e}",
                    sr.source_package
                ));
                SrOutcome::Failed(e.to_string())
            }
        };
    }
}

fn visit_key(item: &Item) -> String {
    format!("{}/{}", item.origin, osc::fuzzy_key(&item.pkg))
}

/// Branch `item` into the branch project, or note that it is already there.
/// The item's origin becomes the project OBS says it was branched from.
fn branch_item(
    branch: &str,
    item: &mut Item,
    follow_devel: bool,
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
    let result = osc::branch_package(&item.origin, &item.pkg, branch, follow_devel, emit)
        .map_err(|e| e.to_string());
    // The project's package listing is cached; it has changed now.
    osc::invalidate_project_packages_cache(branch);
    match result? {
        BranchResult::Branched(b) => {
            if b.target_package != item.pkg {
                // The branch just created a package nothing will build or
                // submit; remove it rather than leave it in the project.
                let cleanup = if b.target_project == branch {
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
                "branched {}/{} to {}/{}",
                b.origin_project, item.pkg, b.target_project, b.target_package
            ));
            item.origin = b.origin_project;
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
        }
    }
    Ok(())
}

/// Check `item`'s BuildRequires against the branch project's paths and add
/// the paths that are missing. Returns the paths chosen and the verdicts.
fn reach_dependencies(
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
fn dependency_items(
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
fn build_ready(
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
        let done = if a.improve {
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
        Ecosystem::Index { .. } | Ecosystem::Repology { .. } => {
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
        improve: improve_problem(item),
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
    fn only_and_exclude_are_comma_lists_matched_by_fuzzy_name() {
        let a = args(&["--only", "python-a,Python_B", "--exclude", "python-b"]);
        assert!(wanted_by(&a.only, &a.exclude, "python-a"));
        assert!(
            !wanted_by(&a.only, &a.exclude, "python-b"),
            "excluded wins over only"
        );
        assert!(!wanted_by(&a.only, &a.exclude, "python-c"));
        // A multibuild flavor is judged by its source package.
        assert!(wanted_by(&a.only, &a.exclude, "python-a:python-a-extras"));
        assert!(wanted_by(&[], &[], "anything"));
        assert!(!wanted_by(&[], &["x".to_string()], "X"));
    }

    #[test]
    fn nothing_is_submitted_or_changed_unless_asked() {
        let a = args(&[]);
        assert!(!a.submit && !a.yes && !a.dry_run && !a.update_deps);
        assert_eq!(a.branch_project, None);
    }

    #[test]
    fn improving_in_place_is_off_unless_asked_and_set_by_the_command() {
        let a = args(&[]);
        assert!(!a.in_place && !a.improve, "improve is never a flag");
        assert!(args(&["--in-place"]).in_place);
    }

    #[test]
    fn the_repository_is_the_projects_own_unless_distro_names_one() {
        assert_eq!(args(&[]).distro, None);
        assert_eq!(
            args(&["--distro", "Leap_16"]).distro.as_deref(),
            Some("Leap_16")
        );
    }

    #[test]
    fn refresh_is_off_unless_asked() {
        assert!(!args(&[]).refresh);
        assert!(args(&["--refresh"]).refresh);
    }

    #[test]
    fn check_is_another_name_for_dry_run() {
        assert!(args(&["--check"]).dry_run);
        assert!(args(&["--dry-run"]).dry_run);
        assert!(!args(&[]).unsafe_resolve);
        assert!(args(&["--unsafe-resolve"]).unsafe_resolve);
    }

    #[test]
    fn the_update_command_names_its_source_with_project_and_has_no_update_project() {
        use clap::Parser as _;
        let parse = |argv: &[&str]| crate::Cli::try_parse_from(argv);
        let Ok(cli) = parse(&["ap", "update", "--project", "Base:System", "--in-place"]) else {
            panic!("update --project --in-place must parse");
        };
        let crate::Commands::Update(a) = cli.command else {
            panic!("not update");
        };
        assert_eq!((a.source.as_str(), a.in_place), ("Base:System", true));
        assert!(parse(&["ap", "update-project", "Base:System"]).is_err());
    }

    #[test]
    fn the_agent_is_told_the_findings_and_to_keep_the_version() {
        let mut item = Item {
            pkg: "libfoo".into(),
            origin: "Base:System".into(),
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/a/b".into(),
            },
            old_version: "1.0".into(),
            new_version: "1.0".into(),
            dependency_of: None,
            findings: Vec::new(),
        };
        assert_eq!(improve_problem(&item), None, "an update has no findings");
        item.findings = vec!["rpmlint-error: devel-file-in-non-devel-package x1".into()];
        let p = improve_problem(&item).unwrap();
        assert!(p.contains("libfoo 1.0"));
        assert!(p.contains("- rpmlint-error: devel-file-in-non-devel-package x1"));
        assert!(p.contains("keep the version"));
    }

    #[test]
    fn min_free_gb_defaults_to_three_and_zero_disables() {
        assert_eq!(args(&[]).min_free_gb, 3);
        assert_eq!(args(&["--min-free-gb", "0"]).min_free_gb, 0);
    }

    #[test]
    fn nvidia_redist_is_off_unless_asked() {
        assert!(!args(&[]).nvidia_redist && args(&[]).nvidia.is_none());
        assert!(args(&["--nvidia-redist"]).nvidia_redist);
        assert_eq!(
            args(&["--nvidia", "redist"]).nvidia,
            Some(crate::NvidiaMode::Redist)
        );
    }
}
