//! The `build` command: one package's dependency closure, built in parallel.

use crate::*;

mod deps;
mod flags;
mod rows;
mod summary;
mod workers;

pub(crate) use deps::*;
pub(crate) use flags::*;
pub(crate) use rows::*;
pub(crate) use summary::*;
pub(crate) use workers::*;
mod spawn;
use spawn::*;
mod ledger;
mod queue;
use ledger::*;
use queue::*;
mod single;
use single::*;
mod round;
use round::*;
mod settle;
use settle::*;
mod frontier;
use frontier::*;
mod resolve;
use resolve::*;

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
    // The requested root once this invocation finishes it. Another invocation
    // in the same process, or a dependency, finishing the same name does not
    // count: a forced root is still built here.
    let mut finished_roots = FinishedRoots::default();

    // Resolve closure, select all buildable targets, build them in parallel,
    // then repeat until the entire closure is built (or an error occurs).
    loop {
        // Before re-resolving, build any runtime-discovered deps (e.g. test-only
        // deps the agent found to be unresolvable).
        // Groups of dependencies set aside until what they wait on has had its
        // batch (see `retry_group` below). The most recent group runs first
        // once nothing else is pending: it waits on the newest packages, and a
        // package queued behind it may wait on it in turn.
        drain_dep_queue(Build {
            project,
            distro,
            arch,
            rpm_name: &rpm_name,
            version: &version,
            cover_flavors: &cover_flavors,
            branch,
            state_root,
            reporter: &reporter,
            pkg_reporter: &pkg_reporter,
            tui_ctrl: &tui_ctrl,
            sem: &sem,
            run_dir,
            dep_drops: &dep_drops,
            patch_requests: &patch_requests,
            native_names: &native_names,
            outdated_names: &outdated_names,
            attempted_deps: &mut attempted_deps,
            deferred_once: &mut deferred_once,
            retrying: &mut retrying,
            dep_rounds: &mut dep_rounds,
            stuck_seeds: &mut stuck_seeds,
            pending_deps: &mut pending_deps,
            config: &mut config,
            max_dep_attempts,
        })?;

        if source_repo.is_some() {
            match single_repo_build(Build {
                project,
                distro,
                arch,
                rpm_name: &rpm_name,
                version: &version,
                cover_flavors: &cover_flavors,
                branch,
                state_root,
                reporter: &reporter,
                pkg_reporter: &pkg_reporter,
                tui_ctrl: &tui_ctrl,
                sem: &sem,
                run_dir,
                dep_drops: &dep_drops,
                patch_requests: &patch_requests,
                native_names: &native_names,
                outdated_names: &outdated_names,
                attempted_deps: &mut attempted_deps,
                deferred_once: &mut deferred_once,
                retrying: &mut retrying,
                dep_rounds: &mut dep_rounds,
                stuck_seeds: &mut stuck_seeds,
                pending_deps: &mut pending_deps,
                config: &mut config,
                max_dep_attempts,
            })? {
                Flow::Done => return Ok(()),
                Flow::Retry => continue,
            }
        }

        let Spawned {
            spawned_any,
            batch_pending,
            batch_failures,
        } = frontier_stream(
            Build {
                project,
                distro,
                arch,
                rpm_name: &rpm_name,
                version: &version,
                cover_flavors: &cover_flavors,
                branch,
                state_root,
                reporter: &reporter,
                pkg_reporter: &pkg_reporter,
                tui_ctrl: &tui_ctrl,
                sem: &sem,
                run_dir,
                dep_drops: &dep_drops,
                patch_requests: &patch_requests,
                native_names: &native_names,
                outdated_names: &outdated_names,
                attempted_deps: &mut attempted_deps,
                deferred_once: &mut deferred_once,
                retrying: &mut retrying,
                dep_rounds: &mut dep_rounds,
                stuck_seeds: &mut stuck_seeds,
                pending_deps: &mut pending_deps,
                config: &mut config,
                max_dep_attempts,
            },
            Round {
                name: &name,
                explicit_target,
                pinned_version: pinned_version.as_deref(),
                tx: &tx,
                rx: &rx,
                in_flight: &mut in_flight,
                fail_counts: &mut fail_counts,
                rate_limited_counts: &mut rate_limited_counts,
                give_ups: &mut give_ups,
                permanently_failed: &mut permanently_failed,
                finished_roots: &mut finished_roots,
                max_target_retries,
            },
        )?;

        if settle_round(
            Build {
                project,
                distro,
                arch,
                rpm_name: &rpm_name,
                version: &version,
                cover_flavors: &cover_flavors,
                branch,
                state_root,
                reporter: &reporter,
                pkg_reporter: &pkg_reporter,
                tui_ctrl: &tui_ctrl,
                sem: &sem,
                run_dir,
                dep_drops: &dep_drops,
                patch_requests: &patch_requests,
                native_names: &native_names,
                outdated_names: &outdated_names,
                attempted_deps: &mut attempted_deps,
                deferred_once: &mut deferred_once,
                retrying: &mut retrying,
                dep_rounds: &mut dep_rounds,
                stuck_seeds: &mut stuck_seeds,
                pending_deps: &mut pending_deps,
                config: &mut config,
                max_dep_attempts,
            },
            Round {
                name,
                explicit_target,
                pinned_version: pinned_version.as_deref(),
                tx: &tx,
                rx: &rx,
                in_flight: &mut in_flight,
                fail_counts: &mut fail_counts,
                rate_limited_counts: &mut rate_limited_counts,
                give_ups: &mut give_ups,
                permanently_failed: &mut permanently_failed,
                finished_roots: &mut finished_roots,
                max_target_retries,
            },
            Spawned {
                spawned_any,
                batch_pending,
                batch_failures,
            },
        )? {
            return Ok(());
        }
    }
}
