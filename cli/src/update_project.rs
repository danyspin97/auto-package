//! `update-project`: update the packages of an OBS project that have a newer
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
use ap_build::update::{self, Ecosystem};
use ap_build::update_project::{self as up, Item, ItemOutcome, ItemResult, SrOutcome};
use ap_core::event::{Event, RunOutcome};
use ap_core::run::RunDir;
use ap_core::sink::{FanoutReporter, HumanSink, JsonlSink, LogLevel, MemorySink, Reporter};
use color_eyre::eyre::eyre;

use super::{
    ProjectLock, Semaphore, catch_panic, cmd_build, install_interrupt_handler, make_semaphore,
    print_run_summary, reject_symlink_project_dir, repo_package_name, report_update_plan, tui,
};

/// How many packages `--update-deps` may pull in, against a dependency chain
/// that keeps growing.
const MAX_DEPENDENCY_ITEMS: usize = 25;

#[derive(clap::Args)]
pub struct UpdateProjectArgs {
    /// OBS project to browse for packages with a newer upstream release
    /// (e.g. Base:System). It is only read, never written.
    pub source: String,

    /// Project to branch the packages into and build them in. Defaults to
    /// `home:<osc user>:branches:<SOURCE>`, what `osc branch` uses. Created by
    /// the first branch when it does not exist.
    #[arg(long)]
    pub branch_project: Option<String>,

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
    #[arg(long, default_value_t = false)]
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

    #[arg(long, default_value = "openSUSE_Tumbleweed")]
    pub distro: String,

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

    /// Maximum number of packages to build concurrently (0 = unlimited). Each
    /// build still runs its own dependency closure, so this bounds the
    /// top-level loop, not every spawned sub-build.
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,

    /// Regenerate each package's spec from scratch instead of carrying the
    /// existing recipe forward (see `update --regen`).
    #[arg(long, default_value_t = false)]
    pub regen: bool,
}

/// What `update-project` changes about how one package is built.
pub struct BranchBuild {
    /// The package's name in the branch project, for a repository package whose
    /// name the pipeline would otherwise guess.
    pub rpm_name: Option<String>,
    /// Looks a missing dependency up on OBS before the pipeline builds it.
    pub resolver: Option<Arc<DepResolver>>,
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
    let branch_project = match &a.branch_project {
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
    if branch_project == a.source {
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
    let _terminal_restore = a.tui.then(tui::TerminalGuard::new);
    let tui_ctrl = if a.tui {
        Some(tui::TuiController::new(memory.clone())?)
    } else {
        None
    };
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
            print_run_summary(&memory, &branch_project, &a.distro, &a.arch),
            up::summary_text(&branch_project, &report.results, &report.paths, a.submit)
        ));
    }

    result?;
    if failed {
        let n = report
            .results
            .iter()
            .filter(|r| matches!(r.outcome, ItemOutcome::Failed(_)))
            .count();
        return Err(eyre!("{n} package(s) failed to update"));
    }
    Ok(())
}

/// The packages whose names `only` and `exclude` allow, as the listing spells
/// them.
fn wanted(a: &UpdateProjectArgs, listed: &str) -> bool {
    let pkg = up::branch_name(listed);
    (a.only.is_empty() || up::name_listed(&a.only, pkg)) && !up::name_listed(&a.exclude, pkg)
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

    // Plan: the same sweep as `update --check`, over the source.
    let candidates =
        update::plan_where(&a.source, &|n| wanted(a, n), emit).map_err(|e| eyre!("{e}"))?;
    if candidates.is_empty() {
        ctx.say(format!("no matching packages in {}", a.source));
        return Ok(());
    }
    report_update_plan(&candidates, ctx.reporter.as_ref());
    let mut items = up::items_from_plan(&a.source, &candidates, &a.exclude);
    if items.is_empty() {
        ctx.say("nothing to update");
        return Ok(());
    }
    items.retain(|item| match up::check_pipeline_name(item) {
        Ok(()) => true,
        Err(why) => {
            ctx.warn(format!("skipping {}: {why}", item.pkg));
            report.results.push(ItemResult {
                item: item.clone(),
                outcome: ItemOutcome::Skipped(why),
                sr: SrOutcome::NotRequested,
            });
            false
        }
    });
    if items.is_empty() {
        return Ok(());
    }

    // A dry run reads the branch project as branching would leave it.
    let resolver = {
        let live = LiveObs {
            reporter: ctx.reporter.clone(),
        };
        let resolver_for = |obs: Box<dyn depreach::Obs>| {
            DepResolver::new(obs, ctx.reporter.clone(), branch, &a.arch)
        };
        if a.dry_run {
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
    let mut queue: VecDeque<Item> = items.into();
    let mut visited: HashSet<String> = queue.iter().map(visit_key).collect();
    let mut ready: Vec<Item> = Vec::new();
    let mut dependency_count = 0;
    let mut branch_checked = false;
    while let Some(mut item) = queue.pop_front() {
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
                        return Err(eyre!(
                            "branch project {branch} has no build repositories; configure one \
                             in its meta before building"
                        ));
                    }
                    Err(e) => return Err(eyre!("could not read the meta of {branch}: {e}")),
                }
            }
        }

        let spec = if a.dry_run {
            update::spec_for_package(&item.origin, &item.pkg, emit)
        } else {
            update::spec_for_package(branch, &item.pkg, emit)
                .or_else(|| update::spec_for_package(&item.origin, &item.pkg, emit))
        };
        match spec {
            Some(spec) => {
                let (adds, verdicts) = reach_dependencies(a, &item, &spec, &resolver, ctx, report)?;
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

    // Build the dependencies before what needs them, each in the branch
    // project like `update` builds in its own.
    ready.sort_by_key(|i| i.dependency_of.is_none());
    for item in ready {
        ctx.say(format!(
            "updating {} to {} in {branch}",
            item.pkg, item.new_version
        ));
        let outcome = match build_item(a, branch, &item, &resolver, ctx) {
            Ok(()) => ItemOutcome::Updated,
            Err(why) => ItemOutcome::Failed(why),
        };
        report.results.push(ItemResult {
            item,
            outcome,
            sr: SrOutcome::NotRequested,
        });
    }

    if a.submit {
        submit_requests(a, branch, &mut report.results, ctx);
    }
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
                osc::get_overall_build_status(branch, &r.item.pkg, &a.distro, &a.arch, emit)
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
                return Err(format!(
                    "OBS branched it as {}, not {}",
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
                Ok(()) => ctx.say(format!("rebased {branch}/{} onto its origin", item.pkg)),
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
    };
    let build = BranchBuild {
        rpm_name: Some(item.pkg.clone()),
        resolver: Some(resolver.clone()),
    };
    catch_panic(|| {
        cmd_build(
            &name,
            Some(&item.new_version),
            branch,
            &ctx.work_dir,
            &ctx.state_root,
            &ctx.rpms_dir,
            &a.distro,
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
        assert!(wanted(&a, "python-a"));
        assert!(!wanted(&a, "python-b"), "excluded wins over only");
        assert!(!wanted(&a, "python-c"));
        // A multibuild flavor is judged by its source package.
        assert!(wanted(&a, "python-a:python-a-extras"));
        assert!(wanted(&args(&[]), "anything"));
        assert!(!wanted(&args(&["--exclude", "x"]), "X"));
    }

    #[test]
    fn nothing_is_submitted_or_changed_unless_asked() {
        let a = args(&[]);
        assert!(!a.submit && !a.yes && !a.dry_run && !a.update_deps);
        assert_eq!(a.branch_project, None);
    }
}
