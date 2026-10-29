//! Command-line surface.

mod build_cmd;
mod cleanup;
mod diffview;
mod run_limits;
mod tui;
mod update_project;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ap_build::pipeline::{FailureKind, PipelineConfig, PipelineResult, run_package, select_target};
use ap_core::event::{Event, PackageOutcome, RunOutcome, SelectionReason};
use ap_core::run::RunDir;
use ap_core::sink::{FanoutReporter, HumanSink, JsonlSink, LogLevel, MemorySink, Reporter};
use clap::{Parser, Subcommand};

use build_cmd::*;

#[derive(Parser)]
#[command(name = "auto-package", about = "Automated RPM packaging for openSUSE")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a package (and report any missing dependencies).
    Build {
        /// PyPI package name.
        name: String,

        /// Pin a specific version; defaults to latest.
        #[arg(long)]
        version: Option<String>,

        /// OBS project (e.g. home:you:python).
        #[arg(long)]
        project: String,

        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,

        /// Directory for per-package notes, shared across projects. Kept outside
        /// work_dir and outside this repository; created on first write.
        #[arg(
            long = "notes",
            alias = "notes-dir",
            default_value = "../auto-package-notes"
        )]
        notes_dir: PathBuf,

        /// Repository to build for (default: the project's own, Tumbleweed
        /// first).
        #[arg(long)]
        distro: Option<String>,

        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// Log level: error, warn, info, debug, trace.
        #[arg(long, default_value = "info")]
        log: String,

        /// Enable interactive TUI mode.
        #[arg(long, default_value_t = false)]
        tui: bool,

        /// Maximum agent fix attempts per package.
        #[arg(long, default_value_t = 10)]
        max_attempts: usize,

        /// USD budget cap for agent calls (total across every turn for
        /// one package, within one CLI invocation — see
        /// `PkgState::cumulative_spent_usd`).
        ///
        /// The cap stops a runaway, not an ordinary hard package: C++ builds
        /// can need 5–10 agent turns to work through missing BuildRequires and
        /// CMake flag discovery, and large closures run many packages through it.
        #[arg(long, default_value_t = 10.0)]
        budget_usd: f64,

        /// Build this specific dependency node instead of the auto-selected one.
        #[arg(long)]
        target: Option<String>,

        /// Rebuild even if state.json already shows this version as built.
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Regenerate the spec from scratch (ignores any existing spec).
        /// Implies `--force`.
        #[arg(long, default_value_t = false)]
        regen: bool,

        /// Agent backend. Only `cli` is accepted; the flag stays so existing
        /// invocations still parse.
        #[arg(long, default_value = "cli")]
        agent_backend: String,

        /// Build only; never call the model.
        #[arg(long, default_value_t = false)]
        no_agent: bool,

        /// Resolve dependencies without the sandbox: pip runs on the host and
        /// may build sdists, running their build backends as you. Without
        /// this, pip runs in bubblewrap, or resolves wheels only when
        /// bubblewrap is unusable. Same as AUTO_PACKAGE_UNSAFE_RESOLVE=1.
        #[arg(long, default_value_t = false)]
        unsafe_resolve: bool,

        /// Delete rebuildable caches (rpms/, sdist caches) before building. Keeps
        /// runs/, state/, ledger.json and osc checkouts; refuses when a checkout
        /// has uncommitted changes.
        #[arg(long, default_value_t = false)]
        cleanup: bool,

        /// Maximum number of packages to build concurrently (0 = unlimited).
        #[arg(long, default_value_t = ap_core::limits::DEFAULT_JOBS)]
        jobs: usize,

        #[command(flatten)]
        limits: run_limits::RunLimits,

        /// Wait for this many GiB free on the work directory's disk before a
        /// package starts its heavy stages (0 = never wait).
        #[arg(long, default_value_t = 3)]
        min_free_gb: u64,

        /// How the NVIDIA CUDA libraries (cublas, cudnn, cudart...) are
        /// packaged: `wheel` repackages the `nvidia-*` PyPI wheels as
        /// `python-nvidia-*` (the default), `redist` packages NVIDIA's own
        /// redistributable archives as `cuda-<name>`. Remembered in the work
        /// directory, so a restart keeps the choice; giving the other value
        /// switches it, with a warning.
        #[arg(long, value_enum, value_name = "MODE")]
        nvidia: Option<NvidiaMode>,

        /// Same as `--nvidia redist`.
        #[arg(long, default_value_t = false, conflicts_with = "nvidia")]
        nvidia_redist: bool,

        /// GitHub URL for Rust packages (e.g. https://github.com/BurntSushi/ripgrep).
        /// When provided, triggers the Rust build path: writes `_service` and
        /// runs `osc service ra` to vendor Cargo dependencies before building.
        #[arg(long)]
        url: Option<String>,

        /// Source repository for a package PyPI ships as wheels only, with no
        /// repository in its metadata (most of the nvidia-* CUDA packages).
        ///
        /// Takes `<package>=<url>`, or a bare `<url>` for the package named on
        /// the command line. Repeatable, and remembered in
        /// `<work-dir>/sources.toml` so dependencies resolved in later runs
        /// pick it up without being asked again.
        ///
        /// `<package>=wheel` is not a URL: it repackages the package from its
        /// upstream wheel instead of building it from source, replacing a
        /// source recipe already in the project (the CUDA `torch` wheel
        /// instead of a multi-hour source build).
        #[arg(long, value_name = "[PKG=]URL|PKG=wheel")]
        source_url: Vec<String>,

        /// Also retry every other package that OBS currently reports as
        /// Failed/Unresolvable/Broken in this project, not just the ones
        /// the requested target's own dependency closure needs.
        ///
        /// Off by default so a build request targets exactly what was
        /// asked for: a sweep would broaden the operation beyond the requested
        /// closure and quietly resolve unrelated packages to current upstream
        /// versions.
        #[arg(long, default_value_t = false)]
        sweep: bool,
    },

    /// Update the packages of an OBS project that have a newer upstream
    /// release, in a branch project, and optionally submit them back.
    ///
    /// Browses PROJECT (read only), branches each package with a newer release
    /// into the branch project, makes the BuildRequires that live in other
    /// projects resolvable by adding those projects as build paths of the
    /// branch project, and builds the new versions there with the ordinary
    /// pipeline, with the new version pinned and `--regen` as asked. With
    /// `--submit`, a package that built gets a submit request to the project it
    /// was branched from. With `--in-place` the new versions are built in
    /// PROJECT itself instead, a project you own. `--dry-run` only prints the
    /// plan.
    #[command(mut_arg("source", |a| a.long("project").value_name("PROJECT")))]
    Update(update_project::UpdateProjectArgs),

    /// Check every package of an OBS project and improve its quality.
    ///
    /// Audits each package's spec (style slips, no `%check`, tests dropped to
    /// get it to build), its latest build result and the rpmlint findings of
    /// its build log. Style slips are committed as they are; everything else
    /// goes through the ordinary pipeline, which asks the agent to fix the
    /// findings in the existing recipe at the same version. The fixes are
    /// built in a branch project and, with `--submit`, sent back as requests;
    /// with `--in-place` they are committed into the project itself. `--dry-run`
    /// only prints the audit.
    #[command(mut_arg("source", |a| a.long("project").value_name("PROJECT")))]
    Lint(update_project::UpdateProjectArgs),

    /// Print what auto-package has learned about a package: the build
    /// requirements, test changes and workarounds kept in its notes.
    Notes {
        /// RPM package name, e.g. python-requests.
        name: String,

        /// Directory the notes are kept in (the same one builds use).
        #[arg(
            long = "notes",
            alias = "notes-dir",
            default_value = "../auto-package-notes"
        )]
        notes_dir: PathBuf,
    },

    /// Work with the OBS baselines kept for failed attempts.
    Baseline {
        #[command(subcommand)]
        command: BaselineCommand,
    },

    /// Explain what happened to a package in a work directory: how its version
    /// was chosen, each attempt with the agent's own summary, the outcome, the
    /// cost and the notes kept. Read from the run files only: no model, no
    /// network, nothing is written.
    Explain {
        /// RPM package name, e.g. aws-c-http.
        name: String,

        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,

        /// OBS project whose work directory to read (needed when the work
        /// directory holds several projects that know the package).
        #[arg(long)]
        project: Option<String>,

        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// Run id to explain (default: the latest run that touched the package).
        #[arg(long)]
        run: Option<String>,

        /// Directory the package notes are kept in.
        #[arg(
            long = "notes",
            alias = "notes-dir",
            default_value = "../auto-package-notes"
        )]
        notes_dir: PathBuf,

        /// Print the same data as JSON.
        #[arg(long)]
        json: bool,
    },
}
mod baseline;
use baseline::*;
mod resolve;
use resolve::*;

/// The longest an interrupted run waits for the OBS writes still in progress
/// (package runs, restores, update commits). A restore is one `osc addremove`
/// and one `osc ci`, each capped by the restore timeout, so this covers a
/// restore that runs to its cap. Past it the process exits: a baseline that was
/// not put back stays on disk marked pending, and the next run of that package
/// restores it before writing.
const RESTORE_WAIT: std::time::Duration =
    std::time::Duration::from_secs(2 * ap_build::proc::RESTORE_TIMEOUT.as_secs() + 30);

/// Clean process-wide shutdown for Ctrl-C — restore the terminal if the TUI
/// had it, print token usage so far, and actually end the process.
///
/// Called from two places: the `ctrlc` crate's real-SIGINT handler (no TUI,
/// or a TUI that for some reason isn't in raw mode) and, directly, from the
/// TUI's own key-handling loop. Raw mode disables signal generation for
/// Ctrl-C entirely, so while the TUI is running the OS never delivers SIGINT
/// at all — Ctrl-C only reaches the process as an ordinary
/// `KeyCode::Char('c')` + `CONTROL` keypress, which is why the TUI cannot
/// just rely on the handler below firing and must call this itself. Either
/// way this has to actually terminate the whole process, not merely stop
/// rendering — the pipeline's package-build threads keep running otherwise,
/// unaffected by whatever the TUI's own event loop does.
fn handle_interrupt(tui_active: bool, memory: &MemorySink) -> ! {
    // A second interrupt while the first one waits for restores ends the wait.
    static INTERRUPTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if INTERRUPTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(130);
    }
    // Stop the build workers (own process groups, so the terminal's Ctrl-C
    // never reached them): otherwise `osc`/`claude` outlive this process.
    // Sets the shutdown flag first, so polling loops and the scheduler stop
    // on their next check. A baseline restore is spared: it puts OBS back.
    ap_build::proc::kill_all_groups();
    // The TUI leaves raw mode and the alternate screen on; restore them before
    // printing anything, or it lands in that state. Done before the wait, so
    // a second Ctrl-C is a signal again and not a keypress nobody reads.
    if tui_active {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    }
    let writing = ap_build::proc::obs_writes_in_flight();
    if writing > 0 {
        eprintln!(
            "\nWaiting up to {}s for {writing} OBS write(s) to finish \
             (Ctrl-C again stops waiting)...",
            RESTORE_WAIT.as_secs()
        );
        if !ap_build::proc::wait_for_obs_writes(RESTORE_WAIT) {
            eprintln!(
                "warning: an OBS write did not finish: OBS may keep a failed attempt; \
                 its saved baseline stays in the work directory marked pending, and the \
                 next run of that package restores it first"
            );
        }
    }
    ap_build::proc::remove_private_dir();
    ap_build::upstream_cache::flush();
    ap_core::sink::flush_jsonl_sinks();
    let usage = memory.usage();
    eprintln!(
        "\nInterrupted — tokens: {} in / {} out — cost: ${:.2}",
        usage.input_tokens, usage.output_tokens, usage.cost_usd
    );
    // The shell convention for death by SIGINT; scripts can tell an
    // interrupted run from a failed one.
    std::process::exit(130);
}

/// Route Ctrl-C (and, with ctrlc's `termination` feature, SIGTERM/SIGHUP) to
/// [`handle_interrupt`]. A later call (`--maintenance` runs `update` once per
/// code stream) points the installed handler at the new run instead of failing.
fn install_interrupt_handler(tui_active: bool, memory: Arc<MemorySink>) -> color_eyre::Result<()> {
    type Current = std::sync::Mutex<Option<(bool, Arc<MemorySink>)>>;
    static CURRENT: Current = std::sync::Mutex::new(None);
    let first = CURRENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace((tui_active, memory))
        .is_none();
    if !first {
        return Ok(());
    }
    ctrlc::set_handler(|| {
        let (tui_active, memory) = CURRENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("set before the handler");
        // Off this thread: it waits for restores, and a second Ctrl-C must
        // still reach the handler to end that wait.
        std::thread::spawn(move || {
            handle_interrupt(tui_active, &memory);
        });
    })
    .map_err(|e| color_eyre::eyre::eyre!("failed to install the interrupt handler: {e}"))
}

/// How a run that was shown in the TUI ends, shared by `build` and `update`.
///
/// `settled` is the TUI's verdict ([`tui::TuiController::all_settled`]), read
/// once it has closed. A settled run has every package built, so it exits 0
/// only if its own run succeeded and no failure is left standing. `failures`
/// holds one message per failure that no later build fixed: a retry that
/// built the package has already removed its earlier failure, and a request
/// that failed before it had a row has no row to retry, so it stays here.
/// Any other run stays open until the user quits, and exits with the first
/// error it has: the run's own, then the failed packages', or else the run
/// ended before every package was built.
///
/// `tui_failure` is set when the render loop died before the run closed. No
/// one was watching the run then, so it is judged by what its builds returned,
/// as a plain run would be, instead of as an unsettled TUI run.
fn tui_run_exit(
    settled: bool,
    tui_failure: Option<String>,
    run: color_eyre::Result<()>,
    failures: Vec<String>,
) -> color_eyre::Result<()> {
    if settled {
        return run_and_failures(run, failures);
    }
    if let Some(e) = tui_failure {
        eprintln!("warning: the TUI stopped before the run ended ({e})");
        return run_and_failures(run, failures);
    }
    if run.is_err() || !failures.is_empty() {
        return run_and_failures(run, failures);
    }
    Err(color_eyre::eyre::eyre!(
        "the run ended before every package was built"
    ))
}

/// The run's own error if it has one, else every failure joined into one
/// error, else `Ok`.
fn run_and_failures(run: color_eyre::Result<()>, failures: Vec<String>) -> color_eyre::Result<()> {
    run?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(color_eyre::eyre::eyre!(failures.join("; ")))
    }
}

/// The TUI for a run that asked for it, or `None` when it cannot be shown
/// here (no terminal to take over). Then a warning says so, and the run goes
/// on as a plain one, with text output and no `--tui`.
fn start_tui(
    enable: bool,
    memory: Arc<MemorySink>,
    scope: tui::RunScope,
    accepts_requests: bool,
) -> Option<tui::TuiController> {
    if !enable {
        return None;
    }
    match tui::TuiController::new(memory, scope, accepts_requests) {
        Ok(tui) => Some(tui),
        Err(e) => {
            eprintln!("warning: cannot show the TUI ({e}); printing plain output instead");
            None
        }
    }
}
mod project_lock;
use project_lock::*;
mod nvidia;
use nvidia::*;

/// Exit status of a run stopped because the agent cannot work (claude is not
/// logged in): distinct from 130 (interrupted) and 1 (an ordinary failure).
const EXIT_AGENT_UNAVAILABLE: i32 = 2;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let mut summary: Option<String> = None;
    let result = run(cli, &mut summary);
    ap_build::upstream_cache::flush();
    ap_core::sink::flush_jsonl_sinks();
    ap_build::proc::remove_private_dir();
    // Printed after `run` returns so the TUI has restored the terminal, and
    // whether or not the run failed: a failed run is the one where the list of
    // what was built and what is still queued matters most.
    if let Some(text) = summary {
        print!("{text}");
    }
    // Checked before the interrupt case: an abort also raises the shutdown flag.
    if let Some(reason) = ap_build::proc::abort_reason() {
        eprintln!("\nerror: run stopped: {reason}");
        std::process::exit(EXIT_AGENT_UNAVAILABLE);
    }
    if ap_build::proc::shutting_down() {
        // Ctrl-C (or SIGTERM) stopped the scheduler, which returned an
        // "interrupted" error before the handler's `process::exit(130)` ran.
        // Give the handler a moment to print its usage line and exit, then
        // exit 130 ourselves so the status never reads as an ordinary failure.
        std::thread::sleep(std::time::Duration::from_secs(2));
        std::process::exit(130);
    }
    result
}

/// Set the free-space minimum and warn once, before any TUI takes the
/// terminal, when the work directory's disk is short of room.
/// The repository to build `project` for, from `--distro` or, without it, the
/// project's own. A choice between several is said, so it is not a surprise.
pub(crate) fn choose_distro(requested: Option<&str>, project: &str, arch: &str) -> String {
    let (chosen, others) = ap_build::osc::choose_repository(project, requested, arch);
    if !others.is_empty() {
        // Called per package during an `update`, so while the TUI owns the screen.
        ap_core::diag!(
            "note: {project} builds for {chosen} and {}; using {chosen} (--distro picks another)",
            others.join(", ")
        );
    }
    chosen
}

pub(crate) fn init_disk_guard(
    work_dir: &Path,
    min_free_gb: u64,
    project: &str,
    distro: &str,
    arch: &str,
) {
    ap_build::workspace::set_min_free_gb(min_free_gb);
    if let Some(warning) = ap_build::workspace::low_space_warning(work_dir) {
        eprintln!("warning: {warning}");
    }
    for (name, outcome) in ap_build::pipeline::reconcile_state(&work_dir.join("state")) {
        eprintln!("note: {name} was left unfinished; now {outcome}");
    }
    // What OBS shows for the project, asked once for the reconcile and the self-check.
    let statuses = ap_build::osc::get_project_build_statuses(project, distro, arch).ok();
    // A package state.json calls unfinished while OBS shows it built gets its
    // RPMs fetched and its state corrected.
    if let Some(statuses) = &statuses {
        let lower: std::collections::HashMap<_, _> = statuses
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let rpms_dir = work_dir.join("rpms");
        let _ = std::fs::create_dir_all(&rpms_dir);
        for (name, what) in ap_build::pipeline::reconcile_with_obs(
            &work_dir.join("state"),
            &work_dir.join(project),
            &rpms_dir,
            &lower,
            |checkout| ap_build::osc::has_local_changes(checkout, ap_build::proc::Emit::quiet()),
            |name| {
                ap_build::osc::download_built_rpms(
                    project,
                    name,
                    distro,
                    arch,
                    &rpms_dir,
                    ap_build::proc::Emit::quiet(),
                )
            },
        ) {
            eprintln!("note: {name}: {what}");
        }
    }
    // The self-check: what the checkouts, state.json and OBS disagree about.
    for finding in ap_build::doctor::run(&work_dir.join("state"), &work_dir.join(project), || {
        statuses
    }) {
        eprintln!("warning: {finding}");
    }
}

fn run(cli: Cli, summary: &mut Option<String>) -> color_eyre::Result<()> {
    match cli.command {
        Commands::Build {
            name,
            version,
            project,
            work_dir,
            notes_dir,
            distro,
            arch,
            log,
            tui: enable_tui,
            max_attempts,
            budget_usd,
            target,
            force,
            regen,
            agent_backend,
            no_agent,
            unsafe_resolve,
            cleanup,
            jobs,
            limits,
            min_free_gb,
            nvidia,
            nvidia_redist,
            url,
            source_url,
            sweep,
        } => {
            // `--regen` means "start over": without this, an already-built
            // package returned AlreadyBuilt (state.json / OBS-green fast paths,
            // `select_target`) before the regeneration ever ran, so `--regen`
            // alone was a silent no-op and needed `--force` alongside it.
            let force = force || regen;

            // `--no-agent` promises no model calls at all, including the
            // web-search fallback that locates an upstream for unknown names.
            ap_build::discovery::set_agent_enabled(!no_agent);
            if !no_agent {
                ap_build::agent::ensure_logged_in().map_err(|e| color_eyre::eyre::eyre!(e))?;
            }
            ap_ecosystem::pip::set_unsafe_resolve(unsafe_resolve);
            limits.apply(&work_dir)?;
            ap_build::sources::set_scratch_dir(work_dir.join("cache"));
            ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);
            let distro = choose_distro(distro.as_deref(), &project, &arch);

            // `build https://github.com/owner/repo` — the URL is the source and
            // the repository name is the package name. `--url` does the same
            // thing but lets the package be named separately.
            let (name, source_repo) = match (looks_like_url(&name), url) {
                (true, Some(u)) => {
                    return Err(color_eyre::eyre::eyre!(
                        "the package name is already a URL ({name}); drop --url {u}"
                    ));
                }
                (true, None) => (repo_package_name(&name), Some(name)),
                (false, u) => (name, u),
            };
            validate_package_name(&name).map_err(|e| color_eyre::eyre::eyre!(e))?;

            // Both of these are silent-wrong-behaviour traps otherwise: a
            // mistyped level would quietly log at info, and an unsupported
            // backend would quietly run the CLI one.
            let level: LogLevel = log
                .parse()
                .map_err(|e: String| color_eyre::eyre::eyre!(e))?;
            if agent_backend != "cli" {
                return Err(color_eyre::eyre::eyre!(
                    "unsupported --agent-backend '{agent_backend}'; only 'cli' is available"
                ));
            }

            // `--work-dir` is the root; each OBS project lives in its own
            // subdir inside it (`<work-dir>/<sanitized-project>-<arch>/`),
            // so one root can hold state for several projects (and now
            // several arches) side by side. `--cleanup` scopes to just
            // this project-arch's subdir.
            //
            // The arch suffix keeps architectures apart: running with
            // `--arch x86_64` and then `--arch aarch64` under the same
            // `--work-dir` must not share state (RPMs, source checkouts,
            // state.json).
            //
            // Guarded by an ownership marker (`.auto-package`) written at
            // the project_dir root on first creation. `--cleanup` refuses
            // to recursively remove a project_dir that does not carry the
            // marker — this stops a mistyped `--work-dir` pointing at an
            // unrelated directory (or a stray symlink into one) from being
            // wiped out just because `--cleanup` was passed. For existing
            // project_dirs with no marker, the user has to add it manually
            // if they want the clean-up to proceed.
            let project_dir_name = format!(
                "{}-{}",
                ap_core::run::sanitize_name(&project),
                ap_core::run::sanitize_name(&arch),
            );
            let project_dir = work_dir.join(&project_dir_name);
            const OWNERSHIP_MARKER: &str = ".auto-package";
            reject_symlink_project_dir(&project_dir)?;
            // Only a directory this run creates is ours to mark. Marking one that
            // already existed would let a later --cleanup delete the user's files.
            let created_now = !project_dir.exists();
            std::fs::create_dir_all(&project_dir)?;
            // Taken before --cleanup touches anything: a second run for this
            // project must not delete state under one that is still building.
            let _project_lock = ProjectLock::acquire(&project_dir)?;
            // `--nvidia`: the flag, else what this work directory used before.
            let nvidia_choice = nvidia.or(nvidia_redist.then_some(NvidiaMode::Redist));
            if cleanup && !created_now {
                let marker = project_dir.join(OWNERSHIP_MARKER);
                if !marker.exists() {
                    return Err(color_eyre::eyre::eyre!(
                        "--cleanup refuses to remove {} because it has no \
                         `.auto-package` ownership marker — this directory \
                         may not have been created by auto-package. Delete \
                         it manually if that is wrong, or `touch {}/.auto-package` \
                         first to accept that --cleanup can recursively \
                         remove it on this and later runs.",
                        project_dir.display(),
                        project_dir.display(),
                    ));
                }
                // Only rebuildable caches go; `runs/`, `state/`, `ledger.json`
                // and the osc checkouts stay (see `cleanup`).
                cleanup::run(&project_dir)?;
            }
            // Runs after the --cleanup refusal above, so a refused run does not
            // change the process-wide NVIDIA mode or print its warnings.
            let nvidia_mode = apply_nvidia_mode(&project_dir, nvidia_choice, cleanup)?;
            if created_now {
                // Non-fatal: without the marker, --cleanup just refuses later.
                if let Err(e) = ap_core::fsx::write_atomic(&project_dir.join(OWNERSHIP_MARKER), b"")
                {
                    ap_core::diag!(
                        "warning: could not write the ownership marker in {}: {e}; \
                         --cleanup will refuse this directory",
                        project_dir.display()
                    );
                }
            }

            // Canonicalize so no relative path can reach a subprocess whose
            // cwd is the osc checkout.
            let project_dir = project_dir.canonicalize()?;

            write_nvidia_mode(&project_dir, nvidia_mode, nvidia_choice.is_some())?;
            let rpms_dir = project_dir.join("rpms");
            std::fs::create_dir_all(&rpms_dir)?;
            let state_root = project_dir.join("state");
            std::fs::create_dir_all(&state_root)?;

            // Source overrides: whatever is on disk, plus anything given on
            // this command line, written straight back so the answer is only
            // ever given once.
            let mut source_urls = ap_build::sources::SourceMap::load(&project_dir)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            if !source_url.is_empty() {
                for arg in &source_url {
                    let (pkg, repo) = ap_build::sources::SourceMap::parse_arg(arg, &name)
                        .map_err(|e| color_eyre::eyre::eyre!(e))?;
                    source_urls.insert(&pkg, &repo);
                }
                source_urls
                    .save(&project_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
            }
            let source_urls = Arc::new(source_urls);

            // Which Python flavors this project builds for, read once from
            // `<project-dir>/flavors.toml` if present.
            let python_flavors = Arc::new(
                ap_build::flavors::PythonFlavors::load(&project_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?,
            );

            init_disk_guard(&project_dir, min_free_gb, &project, &distro, &arch);
            let run_dir = RunDir::allocate(&project_dir)?;
            let work_dir = project_dir;

            // Built before the TUI so it can be handed straight to
            // `TuiController::new` — the TUI's raw-mode terminal swallows a
            // real SIGINT (Ctrl-C only reaches it as a keypress), so it has
            // to call `handle_interrupt` itself with the same usage data
            // rather than relying on the `ctrlc` handler below ever firing.
            let memory = std::sync::Arc::new(MemorySink::new());

            // Declared BEFORE `tui_ctrl` so Rust's reverse-of-declaration
            // local-drop order runs it LAST — after `tui_ctrl` and every
            // reporter Arc has dropped (closing every sender into the render
            // thread) and after the render thread's own in-thread teardown
            // had its chance to run. The render thread is detached: on
            // process exit Rust kills it before its own teardown can
            // complete, leaving raw mode and the alternate screen stuck on
            // in the user's shell. This main-thread guard runs on every
            // exit path (normal return, `?` early return, panic unwinding)
            // and the extra teardown is idempotent.
            let mut _terminal_restore = enable_tui.then(tui::TerminalGuard::new);

            // The TUI takes over the terminal, so text output is dropped in
            // favor of it; jsonl/memory logging are unaffected either way.
            // `set_quiet` also silences the ad hoc `diag!` lines scattered
            // through resolution/search code that don't go through a
            // `Reporter` at all — left unguarded they'd print straight over
            // the TUI's alternate screen.
            let tui_ctrl = start_tui(
                enable_tui,
                memory.clone(),
                tui::RunScope {
                    project: project.clone(),
                    targets: vec![target.clone().unwrap_or_else(|| name.clone())],
                },
                true,
            );
            if tui_ctrl.is_none() {
                _terminal_restore = None;
            }
            ap_core::sink::set_quiet(tui_ctrl.is_some());
            tui::set_explain_dir(work_dir.clone());

            let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
            // Always attached: quiet while the TUI owns the terminal, and it
            // starts printing if the TUI dies mid-run (see `tui::PlainOutput`).
            sinks.push(Box::new(tui::PlainOutput::new(HumanSink::new(level))));

            // Always add JSONL logging
            sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
            sinks.push(Box::new(memory.clone()));

            let base_reporter: Arc<dyn Reporter + Send + Sync> =
                Arc::new(FanoutReporter::new(sinks));
            let reporter: Arc<dyn Reporter + Send + Sync> = match &tui_ctrl {
                Some(t) => t.tap(base_reporter.clone(), None),
                None => base_reporter.clone(),
            };

            install_interrupt_handler(tui_ctrl.is_some(), memory.clone())?;

            let argv: Vec<String> = std::env::args().collect();
            reporter.report(&Event::RunStarted {
                run_id: run_dir.run_id.clone(),
                argv,
                work_dir: work_dir.clone(),
            });

            // Fail fast if the OBS project has no `<repository>` entries — otherwise
            // every per-package `osc build` would fail downstream with "no
            // repositories defined" after we had already checked each one out.
            match ap_build::osc::project_has_repositories(
                &project,
                ap_build::proc::Emit::run(reporter.as_ref()),
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(color_eyre::eyre::eyre!(
                        "OBS project '{project}' has no build repositories defined — \
                         configure at least one (e.g. openSUSE_Tumbleweed → openSUSE:Factory) \
                         in its project meta before building."
                    ));
                }
                Err(e) => {
                    return Err(color_eyre::eyre::eyre!(
                        "could not read OBS meta for project '{project}' to verify its \
                         build repositories: {e}"
                    ));
                }
            }

            let start = Instant::now();
            // In TUI mode the user can add more root packages at runtime
            // (`n` key). Each becomes an independent `cmd_build` on its own
            // thread, sharing the reporter/TUI/run directory; overlapping
            // dependencies are serialised per package inside `run_package`.
            // The dispatcher below ends when the TUI exits (its channel
            // disconnects), so quitting the TUI never leaves it waiting.
            let add_rx = tui_ctrl.as_ref().and_then(|t| t.take_add_requests());
            // One semaphore for the whole run: `--jobs` caps concurrent package
            // builds across the root build and every TUI-added root.
            let sem = make_semaphore(jobs);
            // Roots added in the TUI run on their own threads; their failures
            // are collected here so they reach the outcome and exit code. Each
            // is keyed by the name it was requested under, so a retry that
            // builds the package clears the failure it follows, and a later
            // build of that package as a dependency clears it too (see
            // `withdraw_settled`).
            let added_failures: Mutex<Vec<AddFailure>> = Mutex::new(Vec::new());
            let result = {
                let sem = &sem;
                let added_failures = &added_failures;
                let memory = &memory;
                let (project, work_dir, state_root, rpms_dir) =
                    (&project, &work_dir, &state_root, &rpms_dir);
                let (distro, arch, run_dir) = (&distro, &arch, &run_dir);
                let (source_urls, python_flavors) = (&source_urls, &python_flavors);
                let (reporter, base_reporter, tui_ctrl) = (&reporter, &base_reporter, &tui_ctrl);
                std::thread::scope(|scope| {
                    if let Some(add_rx) = add_rx {
                        scope.spawn(move || {
                            for req in add_rx {
                                scope.spawn(move || {
                                    // A retry is sent under the name its row was
                                    // requested with. The row's own key is an RPM name,
                                    // which a build cannot take back (`python-foo` is
                                    // `foo` on PyPI).
                                    let (raw, retry_of) = request_names(req, |row| {
                                        tui_ctrl.as_ref().and_then(|t| t.origin_of(row))
                                    });
                                    let label = raw
                                        .clone()
                                        .or_else(|| retry_of.clone())
                                        .unwrap_or_default();
                                    // Watches whether the build selected anything to start.
                                    let watch = Arc::new(StartWatch::new(reporter.clone()));
                                    // The package's own outcome is reported to the
                                    // base reporter, so it is read from the run's
                                    // record: events from before this retry are not its.
                                    let finished_before = memory.finished().len();
                                    let r = match &raw {
                                        None => Err(color_eyre::eyre::eyre!(NO_REQUEST_NAME)),
                                        Some(raw) => {
                                            let (extra, extra_repo) = if looks_like_url(raw) {
                                                (repo_package_name(raw), Some(raw.clone()))
                                            } else {
                                                (raw.clone(), None)
                                            };
                                            validate_package_name(&extra)
                                                .map_err(|e| color_eyre::eyre::eyre!(e))
                                                .and_then(|()| {
                                                    catch_panic(|| {
                                                        cmd_build(
                                                            &extra,
                                                            None,
                                                            project,
                                                            work_dir,
                                                            state_root,
                                                            rpms_dir,
                                                            distro,
                                                            arch,
                                                            max_attempts,
                                                            budget_usd,
                                                            None,
                                                            false, // force: reuse anything already built
                                                            false, // regen
                                                            no_agent,
                                                            sem.clone(),
                                                            false, // sweep
                                                            extra_repo,
                                                            source_urls.clone(),
                                                            python_flavors.clone(),
                                                            run_dir,
                                                            watch.clone(),
                                                            base_reporter.clone(),
                                                            tui_ctrl.clone(),
                                                            None,
                                                        )
                                                    })
                                                })
                                        }
                                    };
                                    reporter.report(&match &r {
                                        Ok(()) => {
                                            // The package is built, so an earlier
                                            // failure of the same request is fixed.
                                            added_failures
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner())
                                                .retain(|f| f.label != label);
                                            Event::Progress {
                                                step: None,
                                                message: format!("{label}: build finished"),
                                            }
                                        }
                                        Err(e) => {
                                            added_failures
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner())
                                                .push(AddFailure {
                                                    label: label.clone(),
                                                    message: format!("{label}: {e:#}"),
                                                    after: memory.finished().len(),
                                                });
                                            Event::Warning {
                                                step: None,
                                                message: format!("{label}: {e}"),
                                            }
                                        }
                                    });
                                    // A retry that fails without its own row having
                                    // finished would leave the row marked as retrying
                                    // for good (its own worker reports it, but a retry
                                    // may only have started a dependency). Report it
                                    // failed here. A row the retry did finish keeps
                                    // its own outcome.
                                    // A retry that succeeded without starting anything
                                    // (the package was already built) settles its row
                                    // as already built, instead of leaving it marked
                                    // as retrying.
                                    match (&r, retry_of) {
                                        (Err(e), Some(row))
                                            if !finished_since(
                                                &memory.finished(),
                                                finished_before,
                                                &row,
                                            ) =>
                                        {
                                            let reason = if watch.started() {
                                                format!("retry failed: {e:#}")
                                            } else {
                                                format!("retry did not start: {e:#}")
                                            };
                                            reporter.report(&Event::PackageFinished {
                                                pkg: row,
                                                version: String::new(),
                                                outcome: PackageOutcome::Failed {
                                                    reason,
                                                    disk_full: false,
                                                },
                                            });
                                        }
                                        (Ok(()), Some(row)) if !watch.started() => {
                                            reporter.report(&Event::PackageFinished {
                                                pkg: row,
                                                version: String::new(),
                                                outcome: PackageOutcome::AlreadyBuilt,
                                            });
                                        }
                                        _ => {}
                                    }
                                    // Only now, with its last event reported, does the
                                    // TUI stop counting this request as work.
                                    if let Some(t) = tui_ctrl {
                                        t.work_finished();
                                    }
                                });
                            }
                        });
                    }
                    // A panic here must become an `Err`: the scope also joins the
                    // add-request dispatcher, which only ends when the TUI quits,
                    // and the panic text would be invisible in the alternate screen.
                    let first = catch_panic(|| {
                        cmd_build(
                            &name,
                            version.as_deref(),
                            project,
                            work_dir,
                            state_root,
                            rpms_dir,
                            distro,
                            arch,
                            max_attempts,
                            budget_usd,
                            target.as_deref(),
                            force,
                            regen,
                            no_agent,
                            sem.clone(),
                            sweep,
                            source_repo,
                            source_urls.clone(),
                            python_flavors.clone(),
                            run_dir,
                            reporter.clone(),
                            base_reporter.clone(),
                            tui_ctrl.clone(),
                            None,
                        )
                    });
                    // The TUI stays open after the root build until it finds
                    // nothing left to do or is quit, and `result?` below only
                    // runs after that — so a failure (e.g. pip finding no
                    // version for this interpreter) must be shown here, not
                    // left looking like a hang. What to do next is the TUI's to
                    // say once it knows whether everything is built.
                    if tui_ctrl.is_some()
                        && let Err(e) = &first
                    {
                        reporter.report(&Event::Warning {
                            step: None,
                            message: format!("Root build of {name} failed: {e:#}"),
                        });
                    }
                    if let Some(t) = tui_ctrl {
                        t.work_finished();
                        // RunFinished says how the run went, so it waits for the
                        // requests the TUI added as well. It must not wait for the
                        // dispatcher above: that only ends once the TUI has closed,
                        // and the TUI needs RunFinished to close.
                        t.wait_until_idle();
                    }
                    let failed = first.is_err() || {
                        let mut failures = added_failures.lock().unwrap_or_else(|p| p.into_inner());
                        withdraw_settled(&mut failures, memory, tui_ctrl);
                        !failures.is_empty()
                    };
                    // Sum usage from all AgentReply events recorded during the run.
                    reporter.report(&Event::RunFinished {
                        outcome: if failed {
                            RunOutcome::Failed
                        } else {
                            RunOutcome::Success
                        },
                        elapsed: start.elapsed(),
                        usage: memory.usage(),
                    });
                    first
                })
            };

            // Every add request has returned by now (the scope joined them), so
            // this is the run's final word on which failures still stand.
            let mut failures = added_failures
                .into_inner()
                .unwrap_or_else(|p| p.into_inner());
            withdraw_settled(&mut failures, &memory, &tui_ctrl);
            let added_failures: Vec<String> = failures.into_iter().map(|f| f.message).collect();
            // Built as text now and printed by `main`, once the TUI has
            // restored the terminal; printed here it would be wiped.
            *summary = Some(print_run_summary(
                &memory,
                &project,
                &distro,
                &arch,
                &state_root,
            ));

            // Only the TUI takes add requests, so without it there are no
            // added failures to report.
            if let Some(t) = &tui_ctrl {
                // A settled run closes itself after a grace period, so its
                // verdict is only final once the TUI has closed.
                t.wait_closed();
                tui_run_exit(t.all_settled(), t.failure(), result, added_failures)?;
            } else {
                result?;
            }
        }
        Commands::Lint(mut args) => {
            args.improve = true;
            update_project::run(args, summary)?
        }
        Commands::Notes { name, notes_dir } => {
            ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);
            if ap_build::package_notes::path_for(&name).is_none() {
                return Err(color_eyre::eyre::eyre!(
                    "{name:?} is not a valid package name"
                ));
            }
            let text = ap_build::package_notes::read(&name);
            if text.is_empty() {
                println!("no notes for {name} yet in {}", notes_dir.display());
            } else {
                print!("{text}");
            }
        }
        Commands::Explain {
            name,
            work_dir,
            project,
            arch,
            run,
            notes_dir,
            json,
        } => explain_command(
            &name,
            &work_dir,
            project.as_deref(),
            &arch,
            run.as_deref(),
            &notes_dir,
            json,
        )?,
        Commands::Baseline {
            command:
                BaselineCommand::Discard {
                    name,
                    project,
                    arch,
                    work_dir,
                },
        } => {
            let removed = discard_pending_baselines(&work_dir, &project, &arch, &name)?;
            if removed.is_empty() {
                println!("{name}: no pending OBS baseline in {}", work_dir.display());
            }
            for dir in removed {
                println!(
                    "{name}: discarded the pending baseline in {}; OBS is taken as it is",
                    dir.display()
                );
            }
        }
        Commands::Update(args) => update_project::run(args, summary)?,
    }

    Ok(())
}

/// `explain`: print what the work directory says about one package.
fn explain_command(
    name: &str,
    work_dir: &Path,
    project: Option<&str>,
    arch: &str,
    run: Option<&str>,
    notes_dir: &Path,
    json: bool,
) -> color_eyre::Result<()> {
    use std::io::IsTerminal;
    let err = |e: String| color_eyre::eyre::eyre!(e);
    let dir = ap_build::explain::resolve_project_dir(work_dir, project, arch, name).map_err(err)?;
    ap_build::package_notes::set_notes_dir(std::path::absolute(notes_dir)?);
    let package_notes = ap_build::package_notes::path_for(name)
        .map(|_| ap_build::package_notes::read(name))
        .unwrap_or_default();
    let ex = ap_build::explain::explain(&ap_build::explain::Query {
        project_dir: &dir,
        package: name,
        run,
        package_notes,
    })
    .map_err(err)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&ex)?);
        return Ok(());
    }
    let tty = std::io::stdout().is_terminal();
    let width = if tty {
        crossterm::terminal::size().map_or(100, |(w, _)| w as usize)
    } else {
        100
    };
    let color = tty && std::env::var_os("NO_COLOR").is_none();
    print!(
        "{}",
        ap_build::explain::ansi(&ap_build::explain::render(&ex, width.clamp(40, 140)), color)
    );
    Ok(())
}

/// `update --in-place`: rebuild the packages of PROJECT that have a newer upstream
/// release in PROJECT itself, a project the user owns. Nothing is branched or
/// submitted.
fn update_in_place(
    a: update_project::UpdateProjectArgs,
    summary: &mut Option<String>,
) -> color_eyre::Result<()> {
    let update_project::UpdateProjectArgs {
        source: project,
        only,
        exclude,
        work_dir,
        notes_dir,
        distro,
        arch,
        log,
        tui: enable_tui,
        dry_run: check,
        max_attempts,
        budget_usd,
        no_agent,
        unsafe_resolve,
        jobs,
        regen,
        limits,
        ..
    } = a;
    limits.apply(&work_dir)?;
    ap_build::sources::set_scratch_dir(work_dir.join("cache"));
    let distro = choose_distro(distro.as_deref(), &project, &arch);
    ap_build::discovery::set_agent_enabled(!no_agent);
    if !no_agent {
        ap_build::agent::ensure_logged_in().map_err(|e| color_eyre::eyre::eyre!(e))?;
    }
    ap_ecosystem::pip::set_unsafe_resolve(unsafe_resolve);
    ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);

    let level: LogLevel = log
        .parse()
        .map_err(|e: String| color_eyre::eyre::eyre!(e))?;

    // Same per-project-arch subdir layout as Build (see that arm for
    // the rationale): a single `--work-dir` can hold state for multiple OBS
    // projects (and arches) side by side.
    std::fs::create_dir_all(&work_dir)?;
    let project_dir = work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(&project),
        ap_core::run::sanitize_name(&arch),
    ));
    reject_symlink_project_dir(&project_dir)?;
    std::fs::create_dir_all(&project_dir)?;
    let _project_lock = ProjectLock::acquire(&project_dir)?;
    let work_dir = project_dir.canonicalize()?;

    let rpms_dir = work_dir.join("rpms");
    std::fs::create_dir_all(&rpms_dir)?;
    let state_root = work_dir.join("state");
    std::fs::create_dir_all(&state_root)?;

    let source_urls = Arc::new(
        ap_build::sources::SourceMap::load(&work_dir).map_err(|e| color_eyre::eyre::eyre!(e))?,
    );
    let python_flavors = Arc::new(
        ap_build::flavors::PythonFlavors::load(&work_dir)
            .map_err(|e| color_eyre::eyre::eyre!(e))?,
    );

    let run_dir = RunDir::allocate(&work_dir)?;
    let memory = std::sync::Arc::new(MemorySink::new());
    // See the matching guard in `Commands::Build` for the rationale
    // (short version: the TUI render thread is detached, process
    // exit kills it before its own terminal teardown runs, leaving
    // raw mode and the alternate screen stuck on). Declared BEFORE
    // `tui_ctrl` so it drops LAST.
    let mut _terminal_restore = enable_tui.then(tui::TerminalGuard::new);
    let tui_ctrl = start_tui(
        enable_tui,
        memory.clone(),
        tui::RunScope {
            project: project.clone(),
            targets: only.to_vec(),
        },
        false,
    );
    if tui_ctrl.is_none() {
        _terminal_restore = None;
    }
    ap_core::sink::set_quiet(tui_ctrl.is_some());
    tui::set_explain_dir(work_dir.clone());

    let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
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

    let argv: Vec<String> = std::env::args().collect();
    reporter.report(&Event::RunStarted {
        run_id: run_dir.run_id.clone(),
        argv,
        work_dir: work_dir.clone(),
    });

    // Fail fast if the OBS project has no `<repository>` entries — otherwise
    // every per-package `osc build` would fail downstream with "no
    // repositories defined" after we had already checked each one out.
    match ap_build::osc::project_has_repositories(
        &project,
        ap_build::proc::Emit::run(reporter.as_ref()),
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Err(color_eyre::eyre::eyre!(
                "OBS project '{project}' has no build repositories defined — \
                 configure at least one (e.g. openSUSE_Tumbleweed → openSUSE:Factory) \
                 in its project meta before building."
            ));
        }
        Err(e) => {
            return Err(color_eyre::eyre::eyre!(
                "could not read OBS meta for project '{project}' to verify its \
                 build repositories: {e}"
            ));
        }
    }

    let start = Instant::now();
    let result = cmd_update(
        &project,
        &only,
        &exclude,
        &work_dir,
        &state_root,
        &rpms_dir,
        &distro,
        &arch,
        max_attempts,
        budget_usd,
        no_agent,
        make_semaphore(jobs),
        check,
        regen,
        source_urls,
        python_flavors,
        &run_dir,
        reporter.clone(),
        base_reporter.clone(),
        tui_ctrl.clone(),
    );
    if let Some(t) = &tui_ctrl {
        t.work_finished();
    }

    let outcome = if result.is_ok() {
        RunOutcome::Success
    } else {
        RunOutcome::Failed
    };
    reporter.report(&Event::RunFinished {
        outcome,
        elapsed: start.elapsed(),
        usage: memory.usage(),
    });
    // Built as text now and printed by `main`, once the TUI has
    // restored the terminal; printed here it would be wiped.
    *summary = Some(print_run_summary(
        &memory,
        &project,
        &distro,
        &arch,
        &state_root,
    ));

    match &tui_ctrl {
        Some(t) => {
            t.wait_closed();
            tui_run_exit(t.all_settled(), t.failure(), result, Vec::new())?;
        }
        None => result?,
    }
    Ok(())
}

/// The error that ends a build when packages were given up on: each one with
/// what actually blocked it, since "after N failed attempts" is false for a
/// package given up on because the packages it needs cannot be built, and says
/// nothing about which of them or why.
fn build_incomplete_message(give_ups: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = format!(
        "build incomplete — gave up on {} package(s), blocking everything left in the \
         closure that needs them:",
        give_ups.len()
    );
    for (pkg, why) in give_ups {
        let mut lines = why.lines();
        out.push_str(&format!("\n  - {pkg}: {}", lines.next().unwrap_or("")));
        for line in lines {
            out.push_str(&format!("\n    {}", line.trim_start()));
        }
    }
    out
}

/// The version a package was last built at here, when its last outcome was a
/// success. Read from its state file, so it does not depend on OBS indexing.
///
/// This is the only place that parses state.json for the queue. It reads the
/// two fields as loose JSON instead of going through `PkgState` because
/// `PkgState::load` needs a `Workspace` for one exact directory, while the
/// directory here is found by fuzzy name, and `load` also rewrites a corrupt
/// file aside, which a read-only queue check must not do.
fn built_locally(state_root: &Path, rpm_name: &str) -> Option<String> {
    // The RPM spelling of a PyPI name is only a guess (`_` may or may not become
    // `-`), so match the real state directories by fuzzy key instead of building
    // one path from the guess.
    let wanted = ap_build::osc::fuzzy_key(rpm_name);
    let dir = std::fs::read_dir(state_root)
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| ap_build::osc::fuzzy_key(&e.file_name().to_string_lossy()) == wanted)?;
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("state.json")).ok()?).ok()?;
    if state["last_outcome"].as_str() != Some("success") {
        return None;
    }
    state["last_built_version"].as_str().map(str::to_string)
}

/// The outcome a finished package reports, shared by every build path.
fn package_outcome(result: &PipelineResult) -> PackageOutcome {
    match result {
        PipelineResult::Built { .. } => PackageOutcome::Built,
        PipelineResult::AlreadyBuilt { .. } => PackageOutcome::AlreadyBuilt,
        PipelineResult::MissingDeps { deps, .. } => {
            PackageOutcome::MissingDeps { deps: deps.clone() }
        }
        PipelineResult::Failed { reason, kind, .. } => PackageOutcome::Failed {
            reason: reason.clone(),
            disk_full: *kind == FailureKind::DiskFull,
        },
    }
}

/// Scan `project` for packages with a newer upstream release, print the plan,
/// and (unless `--check`) hand each candidate to the ordinary build pipeline
/// pinned to its new version.
///
/// `--force` is always passed to the per-package build (the whole point is
/// to rebuild at a new version, so state.json's "already built at <old>"
/// check must not short-circuit). `--regen` is passed through from the
/// update command's own `--regen`: default off, so a bump preserves the
/// existing recipe's hand-tuned decisions (patches, BuildRequires choices,
/// `%check` skips); the user opts in when upstream has changed build system
/// or dependency layout enough that reusing the old recipe is worse than
/// starting over. Reusing the recipe by default keeps its recipe-specific work.
///
/// Rebuilds run sequentially by default (`--jobs 1`) because each one already
/// kicks off its own dep-closure concurrency inside `cmd_build`; stacking
/// multiple top-level updates on top of that would multiply, not add. A user
/// who knows their machine can take it can raise `--jobs`.
#[allow(clippy::too_many_arguments)]
fn cmd_update(
    project: &str,
    only: &[String],
    exclude: &[String],
    work_dir: &Path,
    state_root: &Path,
    rpms_dir: &Path,
    distro: &str,
    arch: &str,
    max_attempts: usize,
    budget_usd: f64,
    no_agent: bool,
    sem: Option<Semaphore>,
    check: bool,
    regen: bool,
    source_urls: Arc<ap_build::sources::SourceMap>,
    python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    run_dir: &RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
    tui_ctrl: Option<tui::TuiController>,
) -> color_eyre::Result<()> {
    // The project may call the distribution by another name than `--distro`.
    let distro = &ap_build::osc::resolve_repository(project, distro, arch);
    let sweep = Sweep::start(project, only, exclude, reporter.clone());
    // `--check` only reports the plan: the sweep runs to its end and nothing
    // is built.
    let updates: Box<dyn Iterator<Item = ap_build::update::Candidate>> = if check {
        for _ in sweep.updates.iter() {}
        Box::new(std::iter::empty())
    } else {
        Box::new(sweep.updates.iter())
    };
    let mut failures: Vec<(String, String)> = Vec::new();
    for c in updates {
        // The package exists under this name: the pipeline must not rename it
        // after what it makes of the source (a C project that ships Python
        // bindings reads as `python-<name>`).
        let existing = update_project::BranchBuild {
            rpm_name: Some(ap_build::update_project::branch_name(&c.rpm_name).to_string()),
            resolver: None,
            improve: None,
            from_version: None,
        };
        let ap_build::update::Decision::Update {
            ecosystem,
            old_version,
            new_version,
        } = c.decision
        else {
            continue;
        };
        // Found by a lookup the pipeline cannot build from: already reported
        // in the plan.
        if !ecosystem.can_build() {
            continue;
        }
        // The build pipeline wants a plain "name" positional and an optional
        // source-repo URL. For a PyPI update that's just the PyPI name; for a
        // git-forge update it's the repo's final path segment (what
        // `repo_package_name` would compute for the CLI's own URL arg), and
        // we also hand `source_repo` back in so the pipeline fetches the
        // tarball from the forge rather than guessing a PyPI release.
        let (name, source_repo) = match &ecosystem {
            ap_build::update::Ecosystem::Pypi { pypi_name } => (pypi_name.clone(), None),
            ap_build::update::Ecosystem::GitRepo { url } => {
                (repo_package_name(url), Some(url.clone()))
            }
            // A release directory: the spec's own `Source:` names the archive
            // (and its signature) at the new version, so the build is handed the
            // address of the release it packages, under the package's own name.
            ap_build::update::Ecosystem::Index { url } => (
                c.rpm_name.clone(),
                Some(url.replace(&old_version, &new_version)),
            ),
            _ => continue,
        };
        reporter.report(&Event::Progress {
            step: None,
            message: format!("updating {name} to {new_version}"),
        });
        // `--force` always on so the state.json "already built at <old>"
        // check doesn't short-circuit a rebuild at the new version.
        // `--regen` is a passthrough from the caller: off by default so a
        // bump preserves the existing recipe (see this function's doc
        // comment for why that is the safer default), on when the user
        // asked for it. `sweep` stays off: an `update` of package X must
        // not opportunistically start rebuilding every other stuck
        // package in the project.
        let existing = update_project::BranchBuild {
            from_version: Some(old_version),
            ..existing
        };
        let r = cmd_build(
            &name,
            Some(&new_version),
            project,
            work_dir,
            state_root,
            rpms_dir,
            distro,
            arch,
            max_attempts,
            budget_usd,
            None,
            true,  // force
            regen, // passthrough from Update's own --regen
            no_agent,
            sem.clone(),
            false, // sweep: never from inside an update
            source_repo,
            source_urls.clone(),
            python_flavors.clone(),
            run_dir,
            reporter.clone(),
            base_reporter.clone(),
            tui_ctrl.clone(),
            Some(&existing),
        );
        if let Err(e) = r {
            failures.push((name, e.to_string()));
        }
    }

    let plan = sweep.finish().map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    if plan.total() == 0 {
        reporter.report(&Event::Progress {
            step: None,
            message: format!("no package of {project} to update (is it empty, or do --only and --exclude leave nothing?)"),
        });
    } else if plan.n_update == 0 && !check {
        reporter.report(&Event::Progress {
            step: None,
            message: "nothing to update".into(),
        });
    }

    match failures.first() {
        Some((pkg, err)) => Err(color_eyre::eyre::eyre!(
            "{} update(s) failed; first: {pkg} ({err})",
            failures.len()
        )),
        None => Ok(()),
    }
}

/// The plan of an update, reported one package at a time as the sweep finds it.
#[derive(Default)]
struct PlanReport {
    /// Updates the pipeline can build.
    n_update: usize,
    /// Newer versions found where the pipeline cannot build them from.
    n_found_only: usize,
    n_uptodate: usize,
    n_skipped: usize,
}

impl PlanReport {
    fn add(&mut self, c: &ap_build::update::Candidate, reporter: &(dyn Reporter + Send + Sync)) {
        use ap_build::update::Decision;
        let eco_tag = |e: &ap_build::update::Ecosystem| e.tag();
        let line = match &c.decision {
            Decision::Update {
                ecosystem,
                old_version,
                new_version,
            } => {
                let note = if ecosystem.can_build() {
                    self.n_update += 1;
                    ""
                } else {
                    self.n_found_only += 1;
                    ", not built: the pipeline cannot fetch this source yet"
                };
                format!(
                    "[update]  {}  {} → {}  ({}{note})",
                    c.rpm_name,
                    old_version,
                    new_version,
                    eco_tag(ecosystem),
                )
            }
            Decision::UpToDate { ecosystem, version } => {
                self.n_uptodate += 1;
                format!(
                    "[current] {}  {}  ({})",
                    c.rpm_name,
                    version,
                    eco_tag(ecosystem)
                )
            }
            Decision::Skipped(reason) => {
                self.n_skipped += 1;
                format!("[skip]    {}  ({reason})", c.rpm_name)
            }
        };
        reporter.report(&Event::Progress {
            step: None,
            message: line,
        });
    }

    fn total(&self) -> usize {
        self.n_update + self.n_found_only + self.n_uptodate + self.n_skipped
    }

    fn finish(&self, reporter: &(dyn Reporter + Send + Sync)) {
        reporter.report(&Event::Progress {
            step: None,
            message: format!(
                "update plan: {} to update, {} newer but not buildable, {} up to date, {} skipped",
                self.n_update, self.n_found_only, self.n_uptodate, self.n_skipped
            ),
        });
    }
}

/// The sweep of a project for newer releases, running on its own thread so
/// that the packages it finds can be branched and built while it goes on with
/// the rest. Each package that has an update arrives on `updates`.
struct Sweep {
    updates: std::sync::mpsc::Receiver<ap_build::update::Candidate>,
    handle: std::thread::JoinHandle<Result<PlanReport, String>>,
}

impl Sweep {
    fn start(
        project: &str,
        only: &[String],
        exclude: &[String],
        reporter: Arc<dyn Reporter + Send + Sync>,
    ) -> Self {
        let (tx, updates) = std::sync::mpsc::channel();
        let (project, only, exclude) = (project.to_string(), only.to_vec(), exclude.to_vec());
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            ap_build::update::plan_each(
                &project,
                &|n| update_project::wanted_by(&only, &exclude, n),
                ap_build::proc::Emit::run(reporter.as_ref()),
                &mut |c| {
                    plan.add(&c, reporter.as_ref());
                    let is_update = matches!(
                        &c.decision,
                        ap_build::update::Decision::Update { ecosystem, .. } if ecosystem.can_build()
                    );
                    // A closed channel means the run gave up: stop checking.
                    !is_update || tx.send(c).is_ok()
                },
            )?;
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self { updates, handle }
    }

    /// The plan for the one package `--only` names, read at `revision` and
    /// aimed at `to_version` when given (`update --revision/--to-version`). It
    /// arrives on `updates` like a sweep's, so the rest of the run is the same.
    fn start_pinned(
        project: &str,
        only: &[String],
        revision: Option<String>,
        to_version: Option<String>,
        reporter: Arc<dyn Reporter + Send + Sync>,
    ) -> Self {
        let (tx, updates) = std::sync::mpsc::channel();
        let (project, only) = (project.to_string(), only.to_vec());
        let handle = std::thread::spawn(move || {
            let mut plan = PlanReport::default();
            let emit = ap_build::proc::Emit::run(reporter.as_ref());
            let c = ap_build::branch_revision::plan_one(
                &project,
                &|n| update_project::wanted_by(&only, &[], n),
                revision.as_deref(),
                to_version.as_deref(),
                emit,
            )?;
            plan.add(&c, reporter.as_ref());
            if let ap_build::update::Decision::Update {
                ecosystem,
                old_version,
                new_version,
            } = &c.decision
                && ecosystem.can_build()
            {
                if to_version.is_some() || revision.is_some() {
                    reporter.report(&Event::Progress {
                        step: None,
                        message: ap_build::branch_revision::forced_line(
                            &c.rpm_name,
                            old_version,
                            new_version,
                            revision.as_deref(),
                        ),
                    });
                }
                let _ = tx.send(c);
            }
            plan.finish(reporter.as_ref());
            Ok(plan)
        });
        Self { updates, handle }
    }

    /// How the sweep ended, once `updates` has run dry.
    fn finish(self) -> Result<PlanReport, String> {
        drop(self.updates);
        self.handle
            .join()
            .unwrap_or_else(|_| Err("the sweep of the project panicked".to_string()))
    }
}

/// Whether a failed dependency run is queued again within this run. A failure
/// to meet a ship condition is: the package's own condition counters (two
/// patch turns, then the upstream wheel) bound it, so it neither waits for a
/// dependent to ask in a later run nor depends on the round count. Other
/// failures are retried within `max_attempts` rounds, except a spent allowance
/// or a rate limit, which a repeat would only reproduce.
fn dep_failure_retries(kind: FailureKind, rounds: usize, max_attempts: usize) -> bool {
    match kind {
        FailureKind::ConditionUnmet => true,
        FailureKind::Exhausted | FailureKind::RateLimited => false,
        _ => rounds < max_attempts,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod dep_drops_tests;

#[cfg(test)]
mod blocked_summary_tests;

#[cfg(test)]
mod built_locally_tests;

#[cfg(test)]
mod settle_tests;
