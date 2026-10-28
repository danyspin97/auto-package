//! `update`: update the packages of an OBS project that have a newer
//! upstream release, in a branch project, and optionally submit them back.
//!
//! The source project is only ever read (listing, spec fetches for the plan).
//! Packages are branched into the branch project and built there by the
//! ordinary pipeline, exactly as `update` builds in its own project, so the
//! fix loop, notes, budget and locking behave the same.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
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

#[derive(clap::Args, Clone)]
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

    /// Create the submit requests without asking. Refuses, and submits
    /// nothing, when a change needs a review (its "Needs review" lines name
    /// the packages): run without it to review them.
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
    /// work_dir and outside this repository; created on first write.
    #[arg(
        long = "notes",
        alias = "notes-dir",
        default_value = "../auto-package-notes"
    )]
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
    /// terminal, so there is nowhere to ask. Without `--submit` the TUI stays
    /// open once everything is built: `d` shows a package's diff, `S` submits
    /// the selected one and `A` every built one, each after a confirmation.
    #[arg(long, default_value_t = false)]
    pub tui: bool,

    /// `update`: branch this revision of the package (an OBS revision number or
    /// a srcmd5) instead of the newest one, and update from the version that
    /// revision has. Needs exactly one package in `--only`. Fails when the
    /// branch project already has the package, since it would keep another
    /// revision; delete that branch package first to start over.
    #[arg(long, value_name = "REV")]
    pub revision: Option<String>,

    /// `update`: update to this version instead of the newest release found
    /// upstream. It is checked to exist where the package's source lives (the
    /// `Source:` URL with the version swapped in, a forge tag, a PyPI release)
    /// before anything is branched, and may be older than the packaged one (a
    /// downgrade, noted in the plan). Needs exactly one package in `--only`.
    #[arg(long, value_name = "VERSION")]
    pub to_version: Option<String>,

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
    #[arg(long, default_value_t = ap_core::limits::DEFAULT_JOBS)]
    pub jobs: usize,

    #[command(flatten)]
    pub limits: crate::run_limits::RunLimits,

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

    /// Send the packages of `--only` to the maintenance of these code streams
    /// (update projects such as SUSE:SLE-15-SP7:Update, comma separated, or
    /// `all`) instead of a submit request: each code stream's update project is
    /// browsed instead of PROJECT, the change is built in
    /// `home:<user>:branches:<update project>`, and the packages that built get
    /// one maintenance request to the maintenance project, after confirmation
    /// (see `--yes`). Without a value, asks which of the code streams that
    /// maintain the package. Needs `--bug` (or `--no-bug-ref`). By default a
    /// backport (see `--maintenance-mode`), which needs `--backport`.
    #[arg(long, value_name = "CODESTREAM", value_delimiter = ',', num_args = 0..)]
    pub maintenance: Option<Vec<String>>,

    /// `update --maintenance`: `backport` (the default: each code stream keeps
    /// its version and gets the fix of `--backport` as a patch, what
    /// maintenance normally accepts) or `update` (each code stream is updated
    /// to the newest release, or `--to-version`).
    #[arg(long, value_name = "MODE")]
    pub maintenance_mode: Option<String>,

    /// The upstream fix of a maintenance backport (repeatable): a patch file,
    /// an https URL of a `.patch`/`.diff`, a GitHub/GitLab/Gitea commit URL or
    /// GitHub pull request (its `.patch` is fetched), or a commit id of
    /// `--backport-repo`. Stored in each code stream's package as
    /// `CVE-YYYY-NNNN.patch` (or `fix-<commit>.patch`) and added to its spec.
    #[arg(long = "backport", value_name = "SRC")]
    pub backport: Vec<String>,

    /// The https repository (GitHub, GitLab, Gitea) of a `--backport` commit id.
    #[arg(long, value_name = "URL")]
    pub backport_repo: Option<String>,

    /// A command that tells which code streams a maintenance fix affects: run
    /// without a shell as `COMMAND PACKAGE CODESTREAM REFS [BACKPORT...]` for
    /// each code stream; stdout `affected`, `not-affected <reason>` or
    /// `unknown` (anything else, a failure or a timeout is `unknown`). A code
    /// stream it says is not affected is left out, with the reason.
    #[arg(long, value_name = "COMMAND")]
    pub affected_by: Option<String>,

    /// Print the code streams each package of `--only` is maintained in, and
    /// stop. Reads only.
    #[arg(long, default_value_t = false)]
    pub maintenance_list: bool,

    /// A bug reference for the maintenance update (`bsc#1234567`,
    /// `jsc#SLE-12345`, `CVE-2025-1234`); repeatable or comma separated. Added
    /// to the newest changelog entry and the request description.
    #[arg(long = "bug", value_name = "REF", value_delimiter = ',')]
    pub bug: Vec<String>,

    /// Send a maintenance request without a bug reference.
    #[arg(long, default_value_t = false)]
    pub no_bug_ref: bool,

    /// Set for the runs of `--maintenance`: PROJECT is an update project,
    /// which is branched from.
    #[arg(skip)]
    pub maintenance_source: bool,

    /// Set for the backport runs of `--maintenance`: the package is built at
    /// the version it has, with these patches staged for the agent to add.
    #[arg(skip)]
    pub backport_job: Option<Arc<ap_build::backport::Job>>,
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
    /// `update`: the version the package is at, shown beside the new one.
    pub from_version: Option<String>,
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
    /// Reads diffs and makes submit requests for the TUI (see [`SubmitBackend`]).
    backend: Option<Arc<SubmitBackend>>,
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
    if a.maintenance.is_some() || a.maintenance_list {
        return run_maintenance(a, summary);
    }
    if !a.maintenance_source
        && (a.maintenance_mode.is_some()
            || !a.backport.is_empty()
            || a.backport_repo.is_some()
            || a.affected_by.is_some())
    {
        return Err(eyre!(
            "--maintenance-mode, --backport, --backport-repo and --affected-by are for --maintenance runs"
        ));
    }
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
    ap_build::branch_revision::validate_args(
        &a.only,
        a.revision.as_deref(),
        a.to_version.as_deref(),
        a.in_place,
        a.improve,
    )
    .map_err(|e| eyre!("{e}"))?;
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
            "--submit asks for confirmation on the terminal, which the TUI owns: pass --yes, or drop --submit \
             and press S (or A) in the TUI to submit with a confirmation"
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
    // Always attached: quiet while the TUI owns the terminal, and it starts
    // printing if the TUI dies mid-run (see `tui::PlainOutput`).
    sinks.push(Box::new(tui::PlainOutput::new(HumanSink::new(level))));
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
        backend: None,
    };
    // A dry run builds nothing, so there is nothing to diff or submit.
    let ctx = match (&ctx.tui_ctrl, a.dry_run) {
        (Some(t), false) => {
            let backend = Arc::new(SubmitBackend {
                branch: branch_project.clone(),
                distro: a.distro.clone(),
                arch: a.arch.clone(),
                state_root: ctx.state_root.clone(),
                yes: a.yes,
                reporter: reporter.clone(),
                items: Default::default(),
                created: Default::default(),
            });
            // Left open once everything is built, so a request can still be
            // made; with `--submit` the run makes them itself and ends as before.
            t.set_backend(backend.clone(), !a.submit);
            Ctx {
                backend: Some(backend),
                ..ctx
            }
        }
        _ => ctx,
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
        up::failure_lines(&report.results)
    } else {
        Vec::new()
    };
    match &ctx.tui_ctrl {
        Some(t) => {
            t.wait_closed();
            // Requests made in the TUI after the run ended are not in the summary built above.
            let from_tui = ctx
                .backend
                .as_ref()
                .map(|b| b.created_lines())
                .unwrap_or_default();
            if let Some(text) = summary.as_mut()
                && !from_tui.is_empty()
            {
                text.push_str(&format!(
                    "submit requests made from the TUI ({}):\n  {}\n",
                    from_tui.len(),
                    from_tui.join("\n  ")
                ));
            }
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

    // `update` sweeps the source for newer releases and `lint` audits it, each
    // on its own thread. A package that has a release, or findings, is branched
    // and built as it turns up, while the thread goes on with the others.
    let mut mechanical: HashSet<String> = HashSet::new();
    let (mut sweep, mut audit) = (None, None);
    // A maintenance backport builds each package at the version it has: no
    // sweep, the items come straight from the source's specs.
    let backports: Option<std::cell::RefCell<VecDeque<Item>>> = match &a.backport_job {
        Some(_) => Some(std::cell::RefCell::new(
            backport_items(a, emit).map_err(|e| eyre!(e))?.into(),
        )),
        None => None,
    };
    if backports.is_some() {
        // The items are queued above.
    } else if a.improve {
        audit = Some(Audit::start(a, ctx.reporter.clone()));
    } else if a.revision.is_some() || a.to_version.is_some() {
        sweep = Some(Sweep::start_pinned(
            &a.source,
            &a.only,
            a.revision.clone(),
            a.to_version.clone(),
            ctx.reporter.clone(),
        ));
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
        // A maintenance incident builds against the code stream only: its
        // branch gets no build path that the incident would not have.
        if a.maintenance_source {
            resolver.with_frozen_paths(ap_build::maintenance::FROZEN_PATHS_REASON)
        } else {
            resolver
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
            match branch_item(
                branch,
                &mut item,
                a.follow_devel,
                a.revision.as_deref(),
                ctx,
            ) {
                Ok(()) => {
                    // Branching may have changed the origin, so the item is
                    // also recorded under its new key: a later discovery of the
                    // same package from that project is then not processed twice.
                    visited.insert(visit_key(&item));
                }
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
        let plan = match sweep.finish() {
            Ok(plan) => plan,
            Err(e) => {
                // Packages already branched and prepared are still built before the error is returned.
                if !a.dry_run {
                    build_ready(a, branch, &mut ready, &mechanical, &resolver, ctx, report);
                }
                return Err(eyre!("{e}"));
            }
        };
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
        let end = match audit.finish() {
            Ok(end) => end,
            Err(e) => {
                // Packages already branched and prepared are still built before the error is returned.
                if !a.dry_run {
                    build_ready(a, branch, &mut ready, &mechanical, &resolver, ctx, report);
                }
                return Err(eyre!("{e}"));
            }
        };
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

    if a.dry_run
        && let Some(job) = &a.backport_job
    {
        ctx.say(format!(
            "dry run: nothing is branched, built or submitted; {} stays untouched",
            a.source
        ));
        for i in &ready {
            for line in ap_build::backport::dry_run_lines(job, &i.pkg, &i.new_version, branch) {
                ctx.say(line);
            }
        }
        report.paths.clear();
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
        if let Some(r) = &a.revision {
            ctx.say(format!(
                "revision {r} would be branched, and {branch} must not have the package yet"
            ));
        }
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
        submit_requests(a, branch, &mut report.results, ctx).map_err(|e| eyre!("{e}"))?;
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
            let statuses = osc::get_project_build_statuses(&source, &distro, &arch)
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
                    let Ok(spec) = update::planned_spec(&source, pkg, emit) else {
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
    // A file that is absent is removed again on restore; any other read error
    // stops before the spec is edited, rather than being taken for "absent".
    let changes_before = match std::fs::read(&changes_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", changes_path.display())),
    };
    // What OBS holds is kept before the first write, so a failed commit can be
    // undone on OBS as well as in the checkout.
    let baseline = osc::begin_baseline(
        &ctx.work_dir.join(osc::BASELINE_ROOT),
        &dir,
        branch,
        &item.pkg,
    )
    .map_err(|e| format!("cannot keep the OBS baseline of {branch}: {e}"))?;
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
        osc::note_attempt(&baseline, &commit_files).map_err(|e| e.to_string())?;
        osc::commit(
            &dir,
            "Tidy the spec (auto-package lint)",
            &commit_files,
            emit,
        )
        .map_err(|e| e.to_string())
    })();
    let committed = match committed {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&baseline);
            Ok(())
        }
        Err(why) => match osc::restore_baseline(&baseline, emit) {
            Ok(()) => Err(why),
            Err(e) => Err(format!("{why}; the OBS baseline was NOT restored: {e}")),
        },
    };
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
            return Err(format!(
                "{why}; could not restore the checkout of {branch}: {e}"
            ));
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
    confirm_with(question, &|m| ctx.warn(m))
}

/// [`confirm`] with the warning going to `warn`.
fn confirm_with(question: &str, warn: &dyn Fn(&str)) -> bool {
    matches!(
        ask_line(&format!("{question} [y/N] "), warn)
            .as_deref()
            .map(str::to_lowercase)
            .as_deref(),
        Some("y" | "yes")
    )
}

/// A line typed on the terminal after `prompt`, trimmed; `None` without a
/// terminal (which is said through `warn`) or when it cannot be read.
fn ask_line(prompt: &str, warn: &dyn Fn(&str)) -> Option<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        warn("cannot ask for confirmation without a terminal; pass --yes to submit unasked");
        return None;
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(answer.trim().to_string())
}

/// The item of a maintenance backport for a candidate of the plan: the
/// package at the version it has (`old_version == new_version`), whatever
/// upstream released since.
fn backport_item(origin: &str, c: &update::Candidate) -> Result<Item, String> {
    let pkg = up::branch_name(&c.rpm_name);
    let (ecosystem, version) = match &c.decision {
        update::Decision::Update {
            ecosystem,
            old_version,
            ..
        } => (ecosystem, old_version),
        update::Decision::UpToDate { ecosystem, version } => (ecosystem, version),
        update::Decision::Skipped(why) => {
            return Err(format!(
                "{pkg}: cannot be rebuilt for the backport: {why} (the pipeline needs to know its source)"
            ));
        }
    };
    if !ecosystem.can_build() {
        return Err(format!(
            "{pkg}: its source is known only through the {} lookup, which the pipeline cannot rebuild from",
            ecosystem.tag()
        ));
    }
    Ok(Item {
        pkg: pkg.to_string(),
        origin: origin.to_string(),
        ecosystem: ecosystem.clone(),
        old_version: version.clone(),
        new_version: version.clone(),
        dependency_of: None,
        findings: Vec::new(),
    })
}

/// The packages of `--only` in the source of a backport run, each at its own
/// version.
fn backport_items(a: &UpdateProjectArgs, emit: Emit<'_>) -> Result<Vec<Item>, String> {
    let candidates = update::plan_where(&a.source, &|n| wanted_by(&a.only, &a.exclude, n), emit)?;
    let mut items: Vec<Item> = Vec::new();
    for c in &candidates {
        let item = backport_item(&a.source, c)?;
        if !items.iter().any(|i| i.pkg == item.pkg) {
            items.push(item);
        }
    }
    if items.is_empty() {
        return Err(format!("{} has none of {}", a.source, a.only.join(", ")));
    }
    Ok(items)
}

/// Put the backports into the checkout the pipeline builds `item` from (the
/// pipeline checks out into `<work dir>/<project>/<package>` and keeps files it
/// does not track; one the spec does not name is not committed).
fn stage_backport(
    branch: &str,
    item: &Item,
    job: &ap_build::backport::Job,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let dir = osc::checkout(branch, &item.pkg, &ctx.work_dir, emit).map_err(|e| e.to_string())?;
    // A file of the same name in the code stream itself is not ours to replace.
    let release_files = osc::list_package_files(&item.origin, &item.pkg, emit).unwrap_or_default();
    for note in ap_build::backport::backport_patches(&dir, job, &release_files)? {
        ctx.say(format!("{}: {note}", item.pkg));
    }
    Ok(())
}

/// `--maintenance` and `--maintenance-list`: the ordinary run once per code
/// stream, with the update project as PROJECT and its own branch project, then
/// the maintenance requests (see `ap_build::maintenance`).
fn run_maintenance(a: UpdateProjectArgs, summary: &mut Option<String>) -> color_eyre::Result<()> {
    use ap_build::maintenance;
    let say = |m: &str| println!("{m}");
    let warn = |m: &str| eprintln!("warning: {m}");
    // Without a terminal the maintenance run says itself what to pass instead.
    let ask = |q: &str| ask_line(q, &|_| {});
    let confirm = |q: &str| confirm_with(q, &warn);
    let mut nothing = |_: &maintenance::BuildJob<'_>| Ok(());
    if a.maintenance_list {
        let host = maintenance::Host {
            say: &say,
            warn: &warn,
            ask: &ask,
            confirm: &confirm,
            build: &mut nothing,
        };
        return maintenance::list(&a.only, &host).map_err(|e| eyre!(e));
    }
    if a.submit || a.in_place {
        return Err(eyre!(
            "--maintenance sends the change to the maintenance project: it cannot be combined with --submit or --in-place"
        ));
    }
    let mode =
        maintenance::Mode::parse(a.maintenance_mode.as_deref(), a.improve).map_err(|e| eyre!(e))?;
    if mode == maintenance::Mode::Backport && (a.to_version.is_some() || a.revision.is_some()) {
        return Err(eyre!(
            "--to-version and --revision pick a version; a backport keeps each code stream's own: \
             pass --maintenance-mode update for a version update"
        ));
    }
    let codestreams = a.maintenance.clone().unwrap_or_default();
    if !codestreams.is_empty() && !codestreams.iter().any(|c| c == &a.source || c == "all") {
        say(&format!(
            "--maintenance: the code streams are the sources; {} is not browsed",
            a.source
        ));
    }
    let opts = maintenance::Options {
        packages: a.only.clone(),
        codestreams,
        branch_project: a.branch_project.clone(),
        bugs: a.bug.clone(),
        no_bug_ref: a.no_bug_ref,
        dry_run: a.dry_run,
        yes: a.yes,
        distro: a.distro.clone(),
        arch: a.arch.clone(),
        checkout_dir: std::path::absolute(a.work_dir.join("maintenance"))?,
        mode,
        backports: a.backport.clone(),
        backport_repo: a.backport_repo.clone(),
        affected_by: a.affected_by.clone(),
    };
    let mut summaries: Vec<String> = Vec::new();
    // Each code stream is its own ordinary run, from its own package.
    let mut build = |job: &maintenance::BuildJob<'_>| -> Result<(), String> {
        let mut one = a.clone();
        one.source = job.source.to_string();
        one.branch_project = Some(job.branch.to_string());
        one.maintenance = None;
        one.maintenance_source = true;
        one.backport_job = job.backport.cloned();
        let mut s = None;
        let result = run(one, &mut s);
        summaries.extend(s);
        result.map_err(|e| e.to_string())
    };
    let mut host = maintenance::Host {
        say: &say,
        warn: &warn,
        ask: &ask,
        confirm: &confirm,
        build: &mut build,
    };
    let result = maintenance::run(&opts, &mut host);
    if !summaries.is_empty() {
        *summary = Some(summaries.join("\n"));
    }
    result.map_err(|e| eyre!(e))
}

/// The submit request for `item`, after every check that decides whether one
/// may be made: the update succeeded, the package built in the branch project
/// at the new version, and no request from the branch is open already. The
/// reason it may not be made comes back as the `SrOutcome` to record. Shared by
/// `--submit` and the TUI's submit keys, so both ask exactly the same.
fn prepare_request(
    branch: &str,
    distro: Option<&str>,
    arch: &str,
    state_root: &Path,
    item: &Item,
    outcome: &ItemOutcome,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<osc::SubmitRequest, SrOutcome> {
    let emit = Emit::run(reporter);
    let say = |m: String| {
        reporter.report(&Event::Progress {
            step: None,
            message: m,
        })
    };
    let warn = |m: String| {
        reporter.report(&Event::Warning {
            step: None,
            message: m,
        })
    };
    let updated = *outcome == ItemOutcome::Updated;
    // Read before the build status and the spec are checked, so the request is
    // pinned to a revision at least as old as what was checked.
    let source_rev = if updated {
        match osc::source_srcmd5(branch, &item.pkg, emit) {
            Ok(rev) => Some(rev),
            Err(e) => {
                warn(format!(
                    "no request for {}: could not read its revision: {e}",
                    item.pkg
                ));
                return Err(SrOutcome::Failed(format!(
                    "could not read the revision: {e}"
                )));
            }
        }
    } else {
        None
    };
    let build = updated
        .then(|| {
            osc::get_overall_build_status(
                branch,
                &item.pkg,
                &crate::choose_distro(distro, branch, arch),
                arch,
            )
            // Name the repository/arch that blocks, not just its status.
            .map(|(status, at)| match (&status, at) {
                (osc::BuildStatus::Succeeded, _) | (_, None) => status,
                (_, Some(at)) => osc::BuildStatus::Unknown(format!("{status} on {at}")),
            })
            .map_err(|e| {
                warn(format!(
                    "could not read the build status of {}: {e}",
                    item.pkg
                ))
            })
            .ok()
        })
        .flatten();
    let spec_version = updated
        .then(|| update::spec_for_package(branch, &item.pkg, emit).ok())
        .flatten()
        .and_then(|spec| ap_build::spec::parse_header(&spec))
        .map(|h| h.version);
    if let Err(why) = up::sr_eligibility(item, outcome, build.as_ref(), spec_version.as_deref()) {
        say(format!("no request for {}: {why}", item.pkg));
        return Err(SrOutcome::NotEligible(why));
    }
    match osc::open_submit_requests(branch, &item.pkg, &item.origin, emit) {
        Ok(open) => {
            if let Some(first) = open.first() {
                say(format!(
                    "no request for {}: request {} from {branch} to {} is already open",
                    item.pkg, first.id, item.origin
                ));
                return Err(SrOutcome::Existing(first.id.clone()));
            }
        }
        Err(e) => {
            warn(format!(
                "no request for {}: could not check for an open one: {e}",
                item.pkg
            ));
            return Err(SrOutcome::Failed(format!(
                "could not check for an open request: {e}"
            )));
        }
    }
    Ok(osc::SubmitRequest {
        source_project: branch.to_string(),
        source_package: item.pkg.clone(),
        target_project: item.origin.clone(),
        target_package: item.pkg.clone(),
        description: up::sr_description(
            item,
            newest_changes(branch, &item.pkg, emit).as_deref(),
            &ap_build::fix_flags::of_package(state_root, &item.pkg),
            &ap_build::risk_flags::of_package(state_root, &item.pkg),
        ),
        source_rev,
    })
}

/// How long `osc rdiff` may take.
const RDIFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// The difference between a package in the project it was branched from and
/// in the branch project, as `osc rdiff` prints it.
fn rdiff(
    origin: &str,
    pkg: &str,
    branch: &str,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<String, String> {
    for name in [origin, pkg, branch] {
        if !osc::is_valid_obs_name(name) {
            return Err(format!("{name:?} is not a valid OBS name"));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let out = ap_build::proc::capture_with_timeout(
        "osc",
        &["rdiff", origin, pkg, branch],
        &cwd,
        Emit::run(reporter),
        RDIFF_TIMEOUT,
    )
    .map_err(|e| format!("osc rdiff {origin} {pkg} {branch}: {e}"))?;
    if out.success() {
        Ok(out.stdout)
    } else {
        let why = out.stderr.trim();
        Err(format!(
            "osc rdiff {origin} {pkg} {branch} failed: {}",
            if why.is_empty() { "no output" } else { why }
        ))
    }
}

/// The TUI's way to read diffs and submit. Every package that goes to the
/// build is registered here with its origin, and the request is made by
/// [`prepare_request`] and `osc::create_submit_request`, the code `--submit`
/// runs, so the answer cannot differ between the two.
struct SubmitBackend {
    branch: String,
    distro: Option<String>,
    arch: String,
    /// Where each package's state, with its fix-loop flags, is kept.
    state_root: PathBuf,
    /// `--yes` was given: a request for a change that needs a review is refused.
    yes: bool,
    reporter: Arc<dyn Reporter + Send + Sync>,
    items: std::sync::Mutex<std::collections::HashMap<String, Item>>,
    /// `pkg: SR #id` for each request made from the TUI, for the run summary.
    created: std::sync::Mutex<Vec<String>>,
}

impl SubmitBackend {
    fn register(&self, item: &Item) {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(osc::fuzzy_key(&item.pkg), item.clone());
    }

    fn item(&self, row: &str) -> Result<Item, String> {
        self.items
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&osc::fuzzy_key(row))
            .cloned()
            .ok_or_else(|| format!("{row} is not a package of this run's branch project"))
    }

    fn request(&self, row: &str) -> Result<(Item, osc::SubmitRequest), tui::SubmitFailure> {
        let item = self.item(row).map_err(tui::SubmitFailure::Other)?;
        let needing = needing_review(&self.state_root, &[item.pkg.as_str()]);
        if let Some(lines) = yes_refusal(self.yes, &needing) {
            return Err(tui::SubmitFailure::Other(lines.join("; ")));
        }
        // The TUI only offers rows that built; OBS is asked again inside.
        match prepare_request(
            &self.branch,
            self.distro.as_deref(),
            &self.arch,
            &self.state_root,
            &item,
            &ItemOutcome::Updated,
            self.reporter.as_ref(),
        ) {
            Ok(sr) => Ok((item, sr)),
            Err(outcome) => Err(submit_failure(outcome)),
        }
    }

    fn created_lines(&self) -> Vec<String> {
        self.created
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// Why a request was not made, as the TUI shows it.
fn submit_failure(outcome: SrOutcome) -> tui::SubmitFailure {
    match outcome {
        SrOutcome::Existing(id) => tui::SubmitFailure::Existing(id),
        SrOutcome::NotEligible(why) => tui::SubmitFailure::Other(format!("not eligible: {why}")),
        SrOutcome::Failed(why) => tui::SubmitFailure::Other(why),
        SrOutcome::Declined => tui::SubmitFailure::Other("declined".to_string()),
        SrOutcome::NeedsReview => {
            tui::SubmitFailure::Other("needs a review: run without --yes to review it".to_string())
        }
        SrOutcome::Created(id) => tui::SubmitFailure::Other(format!("already created: {id}")),
        SrOutcome::NotRequested => tui::SubmitFailure::Other("not requested".to_string()),
    }
}

impl tui::PackageBackend for SubmitBackend {
    fn diff(&self, row: &str) -> Result<String, String> {
        let item = self.item(row)?;
        rdiff(
            &item.origin,
            &item.pkg,
            &self.branch,
            self.reporter.as_ref(),
        )
    }

    fn preview(&self, row: &str) -> Result<tui::SubmitPreview, tui::SubmitFailure> {
        let (item, sr) = self.request(row)?;
        Ok(tui::SubmitPreview {
            summary: up::sr_confirmation_lines(&self.branch, &[&item])
                .pop()
                .unwrap_or_default(),
            source: format!("{}/{}", sr.source_project, sr.source_package),
            target: format!("{}/{}", sr.target_project, sr.target_package),
            description: sr.description,
        })
    }

    fn submit(&self, row: &str) -> Result<String, tui::SubmitFailure> {
        let (_, sr) = self.request(row)?;
        match osc::create_submit_request(&sr, Emit::run(self.reporter.as_ref())) {
            Ok(id) => {
                self.reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "created request {id}: {}/{} -> {}/{}",
                        sr.source_project, sr.source_package, sr.target_project, sr.target_package
                    ),
                });
                self.created
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!(
                        "{}: request {id} to {}",
                        sr.source_package, sr.target_project
                    ));
                Ok(id)
            }
            Err(e) => {
                self.reporter.report(&Event::Warning {
                    step: None,
                    message: format!(
                        "could not create the request for {}: {e}",
                        sr.source_package
                    ),
                });
                Err(tui::SubmitFailure::Other(e.to_string()))
            }
        }
    }
}

/// Create a submit request, to its origin project, for each package that
/// built at the new version in the branch project.
///
/// Conservative at every step: a package whose build or version cannot be
/// confirmed gets no request; one with an open request from the branch
/// project already gets none either (and a request list that cannot be read
/// counts as unknown, not as empty); and nothing is created before the list
/// is shown and confirmed, unless `--yes`.
fn submit_requests(
    a: &UpdateProjectArgs,
    branch: &str,
    results: &mut [ItemResult],
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let mut pending: Vec<(usize, osc::SubmitRequest)> = Vec::new();
    for (idx, r) in results.iter_mut().enumerate() {
        match prepare_request(
            branch,
            a.distro.as_deref(),
            &a.arch,
            &ctx.state_root,
            &r.item,
            &r.outcome,
            ctx.reporter.as_ref(),
        ) {
            Ok(sr) => pending.push((idx, sr)),
            Err(sr) => r.sr = sr,
        }
    }
    if pending.is_empty() {
        return Ok(());
    }

    let items: Vec<&Item> = pending.iter().map(|(i, _)| &results[*i].item).collect();
    ctx.say(format!("submit requests to create ({}):", items.len()));
    for (line, item) in up::sr_confirmation_lines(branch, &items).iter().zip(&items) {
        ctx.say(format!("  {line}"));
        // What the change adds that needs a look, under the request it is in.
        for risk in ap_build::risk_flags::lines_of_package(&ctx.state_root, &item.pkg) {
            ctx.say(format!("      {risk}"));
        }
    }
    // `--yes` approves the batch without asking, so a batch with a change that
    // needs a review is refused whole: nothing is submitted.
    let names: Vec<&str> = pending
        .iter()
        .map(|(i, _)| results[*i].item.pkg.as_str())
        .collect();
    if let Some(lines) = yes_refusal(a.yes, &needing_review(&ctx.state_root, &names)) {
        for (idx, _) in &pending {
            results[*idx].sr = SrOutcome::NeedsReview;
        }
        for line in &lines {
            ctx.warn(line.clone());
        }
        return Err(lines[0].clone());
    }
    if !a.yes && !confirm(&format!("Create {} submit request(s)?", pending.len()), ctx) {
        for (idx, _) in &pending {
            results[*idx].sr = SrOutcome::Declined;
        }
        return Ok(());
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
    Ok(())
}

/// The packages among `pkgs` whose change needs a review (it has risk flags
/// recorded), with those flags' lines, in the order given.
fn needing_review(state_root: &Path, pkgs: &[&str]) -> Vec<(String, Vec<String>)> {
    pkgs.iter()
        .filter_map(|pkg| {
            let lines = ap_build::risk_flags::lines_of_package(state_root, pkg);
            (!lines.is_empty()).then(|| (pkg.to_string(), lines))
        })
        .collect()
}

/// What `--yes` refuses: when it is given and any package needs a review, the
/// message first, then each such package and its flags, and what to do
/// instead. `None` when the requests may go on (no `--yes`, or nothing needs a
/// review).
fn yes_refusal(yes: bool, needing: &[(String, Vec<String>)]) -> Option<Vec<String>> {
    if !yes || needing.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "--yes refuses to submit: {} package(s) need a review, nothing was submitted; \
         run without --yes to review them",
        needing.len()
    )];
    for (pkg, risks) in needing {
        lines.push(format!("  {pkg}"));
        lines.extend(risks.iter().map(|r| format!("    {r}")));
    }
    Some(lines)
}

fn visit_key(item: &Item) -> String {
    format!("{}/{}", item.origin, osc::fuzzy_key(&item.pkg))
}

/// Whether `package` is absent from the branch project's listing taken before
/// the branch call, so that call is what created it. A listing that could not
/// be read proves nothing, so the package is not treated as created.
fn created_by_branch(before: &Result<Arc<HashSet<String>>, String>, package: &str) -> bool {
    match before {
        Ok(names) => !names.contains(package),
        Err(_) => false,
    }
}

/// Branch `item` into the branch project, or note that it is already there.
/// The item's origin becomes the project OBS says it was branched from.
fn branch_item(
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

    /// A state root where `python-foo` has a risk flag of its last run and
    /// `python-bar` has none.
    fn state_with_a_risk() -> tempfile::TempDir {
        use ap_build::pipeline::PkgState;
        use ap_build::risk_flags::{RiskFlag, settle};
        use ap_build::workspace::Workspace;
        let root = tempfile::tempdir().unwrap();
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        PkgState::update(&ws, |st| {
            settle(
                st,
                "r1",
                vec![RiskFlag::ScriptDependency {
                    added: true,
                    line: "Requires(post): helper".into(),
                }],
            )
        })
        .unwrap();
        Workspace::open(root.path(), "python-bar").unwrap();
        root
    }

    fn pkg_item(pkg: &str) -> Item {
        Item {
            pkg: pkg.into(),
            origin: "Base:System".into(),
            ecosystem: ap_build::update::Ecosystem::GitRepo {
                url: "https://github.com/a/b".into(),
            },
            old_version: "1".into(),
            new_version: "2".into(),
            dependency_of: None,
            findings: Vec::new(),
        }
    }

    #[test]
    fn yes_refuses_a_batch_with_a_package_that_needs_a_review() {
        let root = state_with_a_risk();
        let needing = needing_review(root.path(), &["python-bar", "python-foo"]);
        assert_eq!(
            needing,
            vec![(
                "python-foo".to_string(),
                vec!["RISK: added dependency line: Requires(post): helper".to_string()]
            )]
        );
        let lines = yes_refusal(true, &needing).expect("refused");
        assert!(lines[0].starts_with("--yes refuses to submit: 1 package(s) need a review"));
        assert!(lines[0].ends_with("run without --yes to review them"));
        assert_eq!(lines[1], "  python-foo");
        assert!(lines[2].contains("Requires(post): helper"));
        assert!(!lines.iter().any(|l| l.contains("python-bar")));
    }

    #[test]
    fn clean_packages_and_an_interactive_run_are_not_refused() {
        let root = state_with_a_risk();
        // Only the clean package: nothing needs a review, even with --yes.
        let clean = needing_review(root.path(), &["python-bar"]);
        assert!(clean.is_empty());
        assert_eq!(yes_refusal(true, &clean), None);
        // Without --yes the confirmation asks instead of refusing.
        let flagged = needing_review(root.path(), &["python-foo"]);
        assert_eq!(yes_refusal(false, &flagged), None);
        // A package with no state at all is clean too.
        assert!(needing_review(root.path(), &["python-new"]).is_empty());
    }

    #[test]
    fn the_tui_refuses_a_flagged_request_under_yes_before_asking_obs() {
        use tui::PackageBackend as _;
        let root = state_with_a_risk();
        let backend = |yes: bool| SubmitBackend {
            branch: "home:me:branches:Base".into(),
            distro: None,
            arch: "x86_64".into(),
            state_root: root.path().to_path_buf(),
            yes,
            reporter: Arc::new(ap_core::sink::MemorySink::new()),
            items: Default::default(),
            created: Default::default(),
        };
        let flagged = backend(true);
        flagged.register(&pkg_item("python-foo"));
        let err = flagged.submit("python-foo").unwrap_err();
        assert!(
            matches!(&err, tui::SubmitFailure::Other(m)
                if m.contains("--yes refuses") && m.contains("run without --yes")),
            "{err:?}"
        );
        // Without --yes the request is not refused here (and this test makes
        // no OBS call, so the check is what the refusal is tested against).
        let asked = backend(false);
        asked.register(&pkg_item("python-foo"));
        assert!(needing_review(root.path(), &["python-foo"]).len() == 1);
        assert_eq!(
            yes_refusal(asked.yes, &needing_review(root.path(), &["python-foo"])),
            None
        );
    }

    #[test]
    fn a_refused_request_reaches_the_tui_as_a_reason_or_an_open_request() {
        assert_eq!(
            submit_failure(SrOutcome::Existing("7".into())),
            tui::SubmitFailure::Existing("7".into())
        );
        assert_eq!(
            submit_failure(SrOutcome::NotEligible("it failed".into())),
            tui::SubmitFailure::Other("not eligible: it failed".into())
        );
    }

    #[test]
    fn the_tui_backend_only_knows_the_packages_it_was_given() {
        use tui::PackageBackend as _;
        let b = SubmitBackend {
            branch: "home:me:branches:Base".into(),
            distro: None,
            arch: "x86_64".into(),
            state_root: PathBuf::from("/nonexistent-state-root"),
            yes: false,
            reporter: Arc::new(ap_core::sink::MemorySink::new()),
            items: Default::default(),
            created: Default::default(),
        };
        assert!(b.diff("foo").unwrap_err().contains("not a package"));
        assert!(b.preview("foo").is_err());
        assert!(b.submit("foo").is_err());
        assert!(b.created_lines().is_empty());
    }
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
    fn maintenance_takes_a_list_or_nothing_and_bugs_repeat() {
        let a = args(&[]);
        assert_eq!(a.maintenance, None);
        assert!(!a.maintenance_list && !a.no_bug_ref && !a.maintenance_source && a.bug.is_empty());
        let a = args(&["--maintenance", "--only", "curl"]);
        assert_eq!(a.maintenance, Some(vec![]));
        assert_eq!(a.only, vec!["curl"]);
        let a = args(&[
            "--maintenance",
            "SUSE:SLE-15-SP7:Update,SUSE:SLE-15-SP6:Update",
            "--bug",
            "bsc#1234567",
            "--bug",
            "CVE-2025-1234,jsc#SLE-1",
        ]);
        assert_eq!(
            a.maintenance,
            Some(vec![
                "SUSE:SLE-15-SP7:Update".into(),
                "SUSE:SLE-15-SP6:Update".into()
            ])
        );
        assert_eq!(a.bug, vec!["bsc#1234567", "CVE-2025-1234", "jsc#SLE-1"]);
    }

    #[test]
    fn backport_flags_repeat_without_splitting_urls_and_need_maintenance() {
        let a = args(&[]);
        assert!(a.maintenance_mode.is_none() && a.backport.is_empty() && a.backport_job.is_none());
        let a = args(&[
            "--maintenance",
            "all",
            "--maintenance-mode",
            "update",
            "--backport",
            "https://x.org/a,b.patch",
            "--backport",
            "abc1234",
            "--backport-repo",
            "https://github.com/o/r",
            "--affected-by",
            "/usr/bin/check-affected",
        ]);
        assert_eq!(a.maintenance_mode.as_deref(), Some("update"));
        assert_eq!(a.backport, vec!["https://x.org/a,b.patch", "abc1234"]);
        assert_eq!(a.backport_repo.as_deref(), Some("https://github.com/o/r"));
        assert_eq!(a.affected_by.as_deref(), Some("/usr/bin/check-affected"));
        let mut summary = None;
        let err = run(args(&["--backport", "fix.patch"]), &mut summary).unwrap_err();
        assert!(
            err.to_string().contains("are for --maintenance runs"),
            "{err}"
        );
    }

    #[test]
    fn a_backport_item_keeps_the_packaged_version() {
        let item = |decision| {
            backport_item(
                "SUSE:SLE-15-SP7:Update",
                &update::Candidate {
                    rpm_name: "curl".into(),
                    decision,
                },
            )
        };
        let eco = Ecosystem::GitRepo {
            url: "https://github.com/curl/curl".into(),
        };
        let got = item(update::Decision::Update {
            ecosystem: eco.clone(),
            old_version: "8.6.0".into(),
            new_version: "8.11.0".into(),
        })
        .unwrap();
        assert_eq!(
            (got.old_version.as_str(), got.new_version.as_str()),
            ("8.6.0", "8.6.0")
        );
        assert_eq!(got.origin, "SUSE:SLE-15-SP7:Update");
        let got = item(update::Decision::UpToDate {
            ecosystem: eco,
            version: "8.6.0".into(),
        })
        .unwrap();
        assert_eq!(got.new_version, "8.6.0");
        let err = item(update::Decision::Skipped(update::SkipReason::NoSourceLine)).unwrap_err();
        assert!(err.contains("cannot be rebuilt for the backport"), "{err}");
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

    #[test]
    fn branch_call_owns_only_packages_missing_before_it() {
        let before: Result<Arc<HashSet<String>>, String> =
            Ok(Arc::new(["foo".to_string()].into_iter().collect()));
        assert!(created_by_branch(&before, "bar"));
        assert!(!created_by_branch(&before, "foo"));
        let unknown: Result<Arc<HashSet<String>>, String> = Err("no listing".to_string());
        assert!(!created_by_branch(&unknown, "bar"));
    }
}
