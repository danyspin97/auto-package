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
mod session;
use session::*;
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
mod run;
use run::*;

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
mod update;
use update::*;
mod sweep;
use sweep::*;

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
