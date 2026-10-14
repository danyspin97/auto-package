//! Command-line surface — REDESIGN M1.

mod tui;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ap_build::pipeline::{
    AgentBackend, FailureKind, PipelineConfig, PipelineResult, run_package, select_target,
};
use ap_core::event::{Event, PackageOutcome, RunOutcome, SelectionReason};
use ap_core::run::RunDir;
use ap_core::sink::{FanoutReporter, HumanSink, JsonlSink, LogLevel, MemorySink, Reporter};
use clap::{Parser, Subcommand};

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
        /// work_dir so they survive cleanup.
        #[arg(long = "notes", alias = "notes-dir", default_value = "notes/packages")]
        notes_dir: PathBuf,

        #[arg(long, default_value = "openSUSE_Tumbleweed")]
        distro: String,

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
        /// $0.30 was the Oct 2026 default; drogon-shaped C++ builds
        /// routinely need 5–10 agent turns to work through missing
        /// BuildRequires and CMake flag discovery, and $0.30 burned out
        /// mid-fix in a well-reproduced case. $4.00 followed, and large
        /// closures (open-webui's ML stack) then ran packages into it, so
        /// the default is $10.00: the point of the cap is to stop a runaway,
        /// not to ration an ordinary hard package.
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

        /// Agent backend. Only `cli` is supported in M1.
        #[arg(long, default_value = "cli")]
        agent_backend: String,

        /// Build only; never call the model.
        #[arg(long, default_value_t = false)]
        no_agent: bool,

        /// Remove the work directory before building.
        #[arg(long, default_value_t = false)]
        cleanup: bool,

        /// Maximum number of packages to build concurrently (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        jobs: usize,

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
        #[arg(long, value_name = "[PKG=]URL")]
        source_url: Vec<String>,

        /// Also retry every other package that OBS currently reports as
        /// Failed/Unresolvable/Broken in this project, not just the ones
        /// the requested target's own dependency closure needs.
        ///
        /// Off by default so a build request targets exactly what was
        /// asked for; the Oct 6 review flagged the implicit sweep (then
        /// always-on) as broadening the operation beyond the requested
        /// closure and quietly resolving unrelated packages to current
        /// upstream versions.
        #[arg(long, default_value_t = false)]
        sweep: bool,
    },

    /// Scan an OBS project for packages with a newer upstream release and
    /// optionally rebuild them at the new version.
    ///
    /// `--check` just prints the plan (which packages have updates, and from
    /// what to what) without touching anything; without it, each package
    /// needing an update runs through the ordinary build pipeline with the
    /// new version pinned and `--force --regen` on, so a new release that
    /// changes build system or deps is picked up correctly.
    Update {
        /// Limit the sweep to a single RPM package name (otherwise all
        /// packages in the project are checked).
        name: Option<String>,

        /// OBS project (e.g. home:you:python).
        #[arg(long)]
        project: String,

        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,

        /// Directory for per-package notes, shared across projects. Kept outside
        /// work_dir so they survive cleanup.
        #[arg(long = "notes", alias = "notes-dir", default_value = "notes/packages")]
        notes_dir: PathBuf,

        #[arg(long, default_value = "openSUSE_Tumbleweed")]
        distro: String,

        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// Log level: error, warn, info, debug, trace.
        #[arg(long, default_value = "info")]
        log: String,

        /// Enable interactive TUI mode for the per-package rebuilds.
        #[arg(long, default_value_t = false)]
        tui: bool,

        /// Just report what would be updated; do not run any builds.
        #[arg(long, default_value_t = false)]
        check: bool,

        /// Maximum agent fix attempts per package (passed through to each
        /// rebuild).
        #[arg(long, default_value_t = 10)]
        max_attempts: usize,

        /// USD budget cap for agent calls (per rebuild).
        #[arg(long, default_value_t = 10.0)]
        budget_usd: f64,

        /// Build only; never call the model.
        #[arg(long, default_value_t = false)]
        no_agent: bool,

        /// Maximum number of packages to rebuild concurrently (0 = unlimited).
        /// Each rebuild still runs its own dep closure, so this bounds the
        /// top-level update loop, not every spawned sub-build.
        #[arg(long, default_value_t = 1)]
        jobs: usize,

        /// Regenerate each updated package's spec from scratch instead of
        /// carrying the existing recipe forward.
        ///
        /// Off by default so a version bump preserves established patches,
        /// BuildRequires choices, %check skips and other hand-tuned
        /// decisions — a spec that already built the previous version
        /// correctly is usually the right starting point. Pass this when
        /// an upstream release changes build system or dependency layout
        /// enough that reusing the old recipe is worse than starting over;
        /// the Oct 6 review flagged the previous always-regenerate default
        /// as silently discarding recipe-specific work.
        #[arg(long, default_value_t = false)]
        regen: bool,
    },

    /// Print what auto-package has learned about a package: the build
    /// requirements, test changes and workarounds kept in its notes.
    Notes {
        /// RPM package name, e.g. python-requests.
        name: String,

        /// Directory the notes are kept in (the same one builds use).
        #[arg(long = "notes", alias = "notes-dir", default_value = "notes/packages")]
        notes_dir: PathBuf,
    },
}

/// Whether a positional target is a repository URL rather than a package name.
fn looks_like_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://") || s.starts_with("git@")
}

/// The package name implied by a repository URL: its repository name.
///
/// `https://github.com/BurntSushi/ripgrep.git` → `ripgrep`; sub-paths
/// (`…/tree/main`), queries and fragments do not leak into the name.
fn repo_package_name(url: &str) -> String {
    // A direct tarball URL names the project in its file name, not as the last
    // path segment (`…/hello-2.12.tar.gz` is `hello`, not the file).
    if ap_build::sources::source_kind(url) == ap_build::sources::SourceKind::DirectArchive
        && let Some((name, _)) = url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
            .and_then(ap_build::sources::archive_name_version)
    {
        return name;
    }
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(forge) = ap_ecosystem::forge::parse(bare)
        && !forge.repo_name.is_empty()
    {
        return forge.repo_name;
    }
    bare.trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(bare)
        .to_string()
}

/// Reject package names that would be unsafe or meaningless downstream (they
/// flow into osc argv, state paths and pip): empty, leading `-`, or containing
/// `/`, whitespace or control characters.
fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("package name is empty".to_string());
    }
    if name.starts_with('-') {
        return Err(format!(
            "invalid package name '{name}': must not start with '-'"
        ));
    }
    if name.contains('/') || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "invalid package name '{name}': must not contain '/', whitespace or control characters"
        ));
    }
    Ok(())
}

/// Produce candidate upstream-project names for a dep the agent (or OBS)
/// reported in RPM BuildRequires shape, from most-specific to most-stripped.
///
/// The agent is instructed to translate shapes like `libtrantor-devel` or
/// `yaml-cpp-devel` into the upstream project name (`trantor`, `yaml-cpp`)
/// before reporting them. In practice some turns don't do that — the dep
/// then arrives at the source-repo discovery step with the RPM cruft
/// still attached, which doesn't match any real upstream project name
/// and `find_source_repo_candidates` returns no verified candidates.
///
/// We try the original name first (unchanged behavior for correctly-
/// translated deps), then progressively strip the common RPM-library
/// decorations:
///   - `-devel` / `-dev` / `-headers` suffix → the development
///     subpackage of a shared library;
///   - `lib` prefix → the sonamed library package itself.
///
/// Returns the list in try-order, deduped, with the original always
/// first so a one-shot `is_verified` check on `candidates[0]` keeps the
/// pre-fix behavior for already-canonical deps.
fn upstream_name_candidates(dep: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![dep.to_string()];
    let suffixes = ["-devel", "-dev", "-headers"];
    let mut stripped = dep.to_string();
    for s in &suffixes {
        if let Some(rest) = stripped.strip_suffix(s) {
            stripped = rest.to_string();
            break;
        }
    }
    if stripped != dep && !out.contains(&stripped) {
        out.push(stripped.clone());
    }
    if let Some(rest) = stripped.strip_prefix("lib")
        && !rest.is_empty()
    {
        let rest = rest.to_string();
        if !out.contains(&rest) {
            out.push(rest);
        }
    }
    out
}

/// Resolve a bare dependency name (already stripped of any `python-` RPM
/// prefix) discovered at runtime into a buildable [`PipelineConfig`].
///
/// For Python parents: tries PyPI first — the overwhelmingly common case —
/// then falls back to discovering a public source repository the same way a
/// wheel-only PyPI release does (`find_source_repo_candidates`).
///
/// For non-Python parents (plain C/C++, Rust): skips PyPI and goes straight to
/// source-repo discovery. A C++ library like `trantor` (drogon's TCP backend)
/// collides on PyPI with an unrelated Python package of the same name — PyPI
/// first would silently queue `python-trantor` from that unrelated project
/// instead of the C++ library the parent actually links against. The parent's
/// ecosystem is read off `config.rpm_name`'s `python-` prefix, which the
/// upstream probe sets through `Ecosystem::rpm_name` (see `build/src/sources.rs`).
///
/// Returns `None` — after reporting a `Warning` event explaining why — when
/// neither source has anything. A closed-source/proprietary dependency
/// (NVIDIA's CUDA toolkit `cuda-devel`/`cuda-runtime` are the running
/// example convention 27 already names) has no public source of any kind and
/// must already be provided by the target OBS project's own repository
/// configuration; this tool cannot manufacture one no matter how many times
/// the agent reports it.
fn resolve_dependency_config(
    dep: &str,
    config: &PipelineConfig,
    reporter: &dyn Reporter,
) -> Option<PipelineConfig> {
    let parent_is_python = config.rpm_name.starts_with("python-");

    if parent_is_python {
        // Use the version-only path, not fetch_latest_release: a wheel-only
        // release (NVIDIA CUDA toolkit packages, Triton, etc.) has a real
        // info.version but no sdist, and fetch_latest_release would reject
        // it on the sdist check — leaving the dependency unqueued even
        // though PyPI could tell us its version perfectly well.
        let pypi_version = ap_ecosystem::pypi::fetch_latest_version(dep).ok();

        if let Some(version) = pypi_version {
            ap_core::diag!(
                "  queuing missing dependency: {} (PyPI: {dep})",
                ap_build::osc::rpm_name(dep)
            );
            return Some(PipelineConfig {
                pkg_name: dep.to_string(),
                rpm_name: ap_build::osc::rpm_name(dep),
                version,
                // A PyPI-resolved dependency never comes from a repository, even
                // if the parent package the agent found it from did.
                source_repo: None,
                subprojects: Vec::new(),
                ..config.clone()
            });
        }
    }

    // Non-Python parent, or Python parent whose dep has no PyPI release:
    // discover a public source repository and probe it. The probe decides
    // whether it builds under Python/Rust/PlainC — one more layer of
    // insurance that the queued dep carries the right RPM name regardless of
    // what the parent's ecosystem was.
    //
    // Try candidate upstream-project spellings in sequence: the agent is
    // supposed to translate OBS's BuildRequires-shape name
    // (`libtrantor-devel`) into the upstream project name (`trantor`)
    // before reporting, but doesn't always — the drogon case landed
    // `trantor-devel` here, which `find_source_repo_candidates` couldn't
    // match. Canonicalize mechanically so a partially-translated dep
    // still has a chance.
    //
    // For EACH candidate name, try EACH verified repo URL — the search
    // can return several same-named repos (three different `trantor`
    // projects on GitHub, for example) and the first one may be a
    // dead fork with no tags. Only give up once no verified URL for any
    // candidate name yields a usable release.
    let mut all_attempts: Vec<String> = Vec::new();
    let mut probe_failure_warnings: Vec<String> = Vec::new();
    let name_candidates = upstream_name_candidates(dep);
    for cand in &name_candidates {
        let repos = ap_build::pipeline::find_source_repo_candidates(cand);
        for repo_url in &repos.verified {
            all_attempts.push(repo_url.clone());
            let version = match ap_build::sources::latest_release(repo_url) {
                Ok(v) => v,
                Err(e) => {
                    probe_failure_warnings
                        .push(format!("{repo_url}: latest-release lookup failed: {e}"));
                    continue;
                }
            };
            let probe = match ap_build::sources::probe(repo_url, &version, cand) {
                Ok(p) => p,
                Err(e) => {
                    probe_failure_warnings.push(format!("{repo_url}: probe failed: {e}"));
                    continue;
                }
            };
            // Found a verified repo whose latest release probes cleanly.
            if cand != dep {
                ap_core::diag!(
                    "  canonicalized {dep} → {cand} to find a source \
                     repository ({repo_url}); agent should have \
                     translated this name itself (strip lib/-devel/-dev \
                     cruft)"
                );
            }
            // Mirrors `cmd_build`'s explicit --source-url handling: the
            // user-given name wins for a monorepo (it names one specific
            // subproject), otherwise the repository's own declared name
            // wins over our guess.
            let effective_name = if probe.subprojects.len() > 1 {
                cand.clone()
            } else {
                probe.project_name.clone().unwrap_or_else(|| cand.clone())
            };
            let dep_rpm = probe.ecosystem.rpm_name(&effective_name);
            // A devel-wrapped dep (`libarrow-devel`) canonicalized to its
            // umbrella project (`arrow`) can probe straight back to the
            // package currently being built (pyarrow lives inside the
            // apache/arrow monorepo, so `arrow` resolves to `python-pyarrow`
            // again). Accepting that would overwrite this package's own
            // in-flight version/config with the monorepo's git-tag version,
            // which then fails spec generation and loops forever reselecting
            // the same broken version. Skip and keep trying other
            // candidates/repos instead.
            if dep_rpm == config.rpm_name {
                probe_failure_warnings.push(format!(
                    "{repo_url}: resolved back to this package's own rpm_name \
                     ({dep_rpm}) — likely the umbrella project of a devel-wrapped \
                     dependency name; skipping"
                ));
                continue;
            }
            ap_core::diag!(
                "  queuing missing dependency: {dep_rpm} \
                 (source repository: {repo_url}, not on PyPI)"
            );
            return Some(PipelineConfig {
                pkg_name: effective_name,
                rpm_name: dep_rpm,
                version,
                source_repo: Some(repo_url.clone()),
                subprojects: probe.subprojects,
                ..config.clone()
            });
        }
    }

    // No candidate × repo combination yielded a usable release. Build
    // one warning that names every URL we actually tried and why each
    // failed, so the user can see 'yes we tried an-tao/trantor but it
    // returned Y' instead of a bare 'no source repo found'.
    let detail = if probe_failure_warnings.is_empty() {
        format!(
            "no verified source repository found at all (tried names: {})",
            name_candidates.join(", "),
        )
    } else {
        format!(
            "every verified repository failed to resolve a usable release \
             (tried {} URL(s) across {} name candidate(s)):\n  - {}",
            all_attempts.len(),
            name_candidates.len(),
            probe_failure_warnings.join("\n  - ")
        )
    };
    reporter.report(&Event::Warning {
        step: None,
        message: format!(
            "{dep} has no PyPI package and no usable public source \
             repository. {detail}. If this is a system library or a \
             closed-source redistribution (like NVIDIA's \
             cuda-devel/cuda-runtime), it must already be provided by \
             the target project's own repository configuration; this \
             tool has no way to manufacture one. Otherwise, pass \
             --source-url {dep}=<url> to point it at the upstream repo."
        ),
    });
    None
}

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
    // The TUI leaves the terminal in raw mode + the alternate screen;
    // without restoring it first, this message and the shell prompt after
    // exit both land inside that broken state instead of the user's normal
    // screen.
    // Stop the build workers (own process groups, so the terminal's Ctrl-C
    // never reached them): otherwise `osc`/`claude` outlive this process.
    ap_build::proc::kill_all_groups();
    if tui_active {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    }
    let usage = memory.usage();
    eprintln!(
        "\nInterrupted — tokens: {} in / {} out — cost: ${:.4}",
        usage.input_tokens, usage.output_tokens, usage.cost_usd
    );
    std::process::exit(1);
}

/// Route Ctrl-C (a real SIGINT) to [`handle_interrupt`].
fn install_interrupt_handler(tui_active: bool, memory: Arc<MemorySink>) {
    ctrlc::set_handler(move || handle_interrupt(tui_active, &memory))
        .expect("failed to set Ctrl-C handler");
}

/// Name of the per-project lock file, kept inside the project directory.
const PROJECT_LOCK_NAME: &str = ".auto-package.lock";

/// Refuse a project directory that is a symlink: `create_dir_all` and the
/// marker write would follow it and put state (and `--cleanup`'s deletes)
/// wherever it points, e.g. `~/Documents`.
fn reject_symlink_project_dir(dir: &Path) -> color_eyre::Result<()> {
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(color_eyre::eyre::eyre!(
            "{} is a symlink; refusing to use it as a project directory. \
             Remove it, or point --work-dir somewhere else.",
            dir.display()
        ));
    }
    Ok(())
}

/// Advisory per-project lock: an `flock(2)` on a file in the project
/// directory, held for as long as `file` stays open. The kernel releases it
/// when the process exits or is killed, so no stale lock can block a later run.
struct ProjectLock {
    _file: std::fs::File,
}

impl ProjectLock {
    fn acquire(project_dir: &Path) -> color_eyre::Result<Self> {
        let path = project_dir.join(PROJECT_LOCK_NAME);
        match ap_core::proc_lock::try_lock(&path) {
            Ok(file) => Ok(Self { _file: file }),
            Err(ap_core::proc_lock::LockError::Held(pid)) => {
                let who = pid.map_or(String::from("another process"), |p| format!("pid {p}"));
                Err(color_eyre::eyre::eyre!(
                    "another auto-package run ({who}) is using {}; \
                     wait for it to finish.",
                    project_dir.display(),
                ))
            }
            Err(ap_core::proc_lock::LockError::Io(e)) => Err(color_eyre::eyre::eyre!(
                "could not take the project lock {}: {e}",
                path.display()
            )),
        }
    }
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let mut summary: Option<String> = None;
    let result = run(cli, &mut summary);
    // Printed after `run` returns so the TUI has restored the terminal, and
    // whether or not the run failed: a failed run is the one where the list of
    // what was built and what is still queued matters most.
    if let Some(text) = summary {
        print!("{text}");
    }
    result
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
            cleanup,
            jobs,
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
            ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);

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
            // The arch suffix guards against cross-arch bleed — the external
            // 2026-10-06 review flagged that running once with `--arch x86_64`
            // and then with `--arch aarch64` under the same `--work-dir`
            // SHARED state (RPMs, source checkouts, state.json), so each
            // arch's artifacts collided with the other's.
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
            let mut created_now = !project_dir.exists();
            std::fs::create_dir_all(&project_dir)?;
            // Taken before --cleanup touches anything: a second run for this
            // project must not delete state under one that is still building.
            let _project_lock = ProjectLock::acquire(&project_dir)?;
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
                // Empty the directory rather than removing it: it holds our lock
                // file, and the marker is rewritten below.
                for entry in std::fs::read_dir(&project_dir)? {
                    let path = entry?.path();
                    if path.file_name().is_some_and(|n| n == PROJECT_LOCK_NAME) {
                        continue;
                    }
                    if path.is_dir() && !path.is_symlink() {
                        std::fs::remove_dir_all(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                }
                created_now = true;
            }
            if created_now {
                // Non-fatal: without the marker, --cleanup just refuses later.
                let _ = std::fs::write(project_dir.join(OWNERSHIP_MARKER), b"");
            }

            // Canonicalize so no relative path can reach a subprocess whose
            // cwd is the osc checkout.
            let project_dir = project_dir.canonicalize()?;

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
            let _terminal_restore = enable_tui.then(tui::TerminalGuard::new);

            // The TUI takes over the terminal, so text output is dropped in
            // favor of it; jsonl/memory logging are unaffected either way.
            // `set_quiet` also silences the ad hoc `diag!` lines scattered
            // through resolution/search code that don't go through a
            // `Reporter` at all — left unguarded they'd print straight over
            // the TUI's alternate screen.
            let tui_ctrl = if enable_tui {
                Some(tui::TuiController::new(memory.clone())?)
            } else {
                None
            };
            ap_core::sink::set_quiet(tui_ctrl.is_some());

            let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
            if tui_ctrl.is_none() {
                sinks.push(Box::new(HumanSink::new(level)));
            }

            // Always add JSONL logging
            sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
            sinks.push(Box::new(memory.clone()));

            let base_reporter: Arc<dyn Reporter + Send + Sync> =
                Arc::new(FanoutReporter::new(sinks));
            let reporter: Arc<dyn Reporter + Send + Sync> = match &tui_ctrl {
                Some(t) => t.tap(base_reporter.clone(), None),
                None => base_reporter.clone(),
            };

            install_interrupt_handler(tui_ctrl.is_some(), memory.clone());

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
            // are collected here so they reach the outcome and exit code.
            let added_failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
            let result = {
                let sem = &sem;
                let added_failures = &added_failures;
                let (project, work_dir, state_root, rpms_dir) =
                    (&project, &work_dir, &state_root, &rpms_dir);
                let (distro, arch, run_dir) = (&distro, &arch, &run_dir);
                let (source_urls, python_flavors) = (&source_urls, &python_flavors);
                let (reporter, base_reporter, tui_ctrl) = (&reporter, &base_reporter, &tui_ctrl);
                std::thread::scope(|scope| {
                    if let Some(add_rx) = add_rx {
                        scope.spawn(move || {
                            for raw in add_rx {
                                scope.spawn({
                                    let raw = raw.clone();
                                    move || {
                                        let (extra, extra_repo) = if looks_like_url(&raw) {
                                            (repo_package_name(&raw), Some(raw.clone()))
                                        } else {
                                            (raw.clone(), None)
                                        };
                                        let r = validate_package_name(&extra)
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
                                                        reporter.clone(),
                                                        base_reporter.clone(),
                                                        tui_ctrl.clone(),
                                                    )
                                                })
                                            });
                                        reporter.report(&match r {
                                            Ok(()) => Event::Progress {
                                                step: None,
                                                message: format!("{raw}: build finished"),
                                            },
                                            Err(e) => {
                                                added_failures
                                                    .lock()
                                                    .unwrap_or_else(|p| p.into_inner())
                                                    .push(format!("{raw}: {e:#}"));
                                                Event::Warning {
                                                    step: None,
                                                    message: format!("{raw}: {e}"),
                                                }
                                            }
                                        });
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
                        )
                    });
                    if tui_ctrl.is_some() {
                        // The TUI stays open after the root build, and `result?`
                        // below only runs once it quits — so a failure (e.g. pip
                        // finding no version for this interpreter) must be shown
                        // here, not left looking like a hang.
                        match &first {
                            Ok(_) => reporter.report(&Event::Progress {
                                step: None,
                                message: "Root build finished — press n to add a package, q to quit"
                                    .to_string(),
                            }),
                            Err(e) => reporter.report(&Event::Warning {
                                step: None,
                                message: format!(
                                    "Root build of {name} failed: {e:#} — press n to add a package, q to quit"
                                ),
                            }),
                        }
                    }
                    first
                })
            };

            let added_failures = added_failures
                .into_inner()
                .unwrap_or_else(|p| p.into_inner());
            let outcome = if result.is_ok() && added_failures.is_empty() {
                RunOutcome::Success
            } else {
                RunOutcome::Failed
            };
            // Sum usage from all AgentReply events recorded during the run.
            reporter.report(&Event::RunFinished {
                outcome,
                elapsed: start.elapsed(),
                usage: memory.usage(),
            });
            // Built as text now and printed by `main`, once the TUI has
            // restored the terminal; printed here it would be wiped.
            *summary = Some(print_run_summary(&memory, &project, &distro, &arch));

            result?;
            if !added_failures.is_empty() {
                return Err(color_eyre::eyre::eyre!(
                    "{} added package(s) failed: {}",
                    added_failures.len(),
                    added_failures.join("; ")
                ));
            }
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
        Commands::Update {
            name: only,
            project,
            work_dir,
            notes_dir,
            distro,
            arch,
            log,
            tui: enable_tui,
            check,
            max_attempts,
            budget_usd,
            no_agent,
            jobs,
            regen,
        } => {
            ap_build::discovery::set_agent_enabled(!no_agent);
            ap_build::package_notes::set_notes_dir(std::path::absolute(&notes_dir)?);

            let level: LogLevel = log
                .parse()
                .map_err(|e: String| color_eyre::eyre::eyre!(e))?;

            // Same per-project-arch subdir layout as Build (see that arm for
            // the rationale, including the Oct 6 cross-arch-bleed review) — a
            // single `--work-dir` can hold state for multiple OBS projects
            // (and arches) side by side.
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
                ap_build::sources::SourceMap::load(&work_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?,
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
            let _terminal_restore = enable_tui.then(tui::TerminalGuard::new);
            let tui_ctrl = if enable_tui {
                Some(tui::TuiController::new(memory.clone())?)
            } else {
                None
            };
            ap_core::sink::set_quiet(tui_ctrl.is_some());

            let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();
            if tui_ctrl.is_none() {
                sinks.push(Box::new(HumanSink::new(level)));
            }
            sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));
            sinks.push(Box::new(memory.clone()));

            let base_reporter: Arc<dyn Reporter + Send + Sync> =
                Arc::new(FanoutReporter::new(sinks));
            let reporter: Arc<dyn Reporter + Send + Sync> = match &tui_ctrl {
                Some(t) => t.tap(base_reporter.clone(), None),
                None => base_reporter.clone(),
            };

            install_interrupt_handler(tui_ctrl.is_some(), memory.clone());

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
                only.as_deref(),
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
            *summary = Some(print_run_summary(&memory, &project, &distro, &arch));

            result?;
        }
    }

    Ok(())
}

/// A counting semaphore that limits how many package builds run concurrently.
///
/// `None` means unlimited. One instance is created per Build/Update command
/// and shared by every `cmd_build` it spawns (including TUI-added roots), so
/// `--jobs` is a global cap. Threads block in `acquire` until a slot is free.
struct Limiter {
    limit: usize,
    active: std::sync::Mutex<usize>,
    cvar: std::sync::Condvar,
}

type Semaphore = Arc<Limiter>;

fn make_semaphore(jobs: usize) -> Option<Semaphore> {
    (jobs != 0).then(|| {
        Arc::new(Limiter {
            limit: jobs,
            active: std::sync::Mutex::new(0),
            cvar: std::sync::Condvar::new(),
        })
    })
}

/// A held semaphore slot, released on drop so a panicking build cannot leak it.
struct Permit(Option<Semaphore>);

impl Permit {
    fn acquire(sem: &Option<Semaphore>) -> Permit {
        if let Some(s) = sem {
            // A poisoned lock only means another holder panicked; the count
            // itself is still consistent because release happens in `drop`.
            let mut active = s.active.lock().unwrap_or_else(|e| e.into_inner());
            while *active >= s.limit {
                active = s.cvar.wait(active).unwrap_or_else(|e| e.into_inner());
            }
            *active += 1;
        }
        Permit(sem.clone())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            *s.active.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
            s.cvar.notify_one();
        }
    }
}

/// Best-effort message from a `catch_unwind` / `JoinHandle::join` panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Run `f`, converting a panic into an `Err` carrying the panic message.
fn catch_panic<T>(f: impl FnOnce() -> color_eyre::Result<T>) -> color_eyre::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        Err(color_eyre::eyre::eyre!(
            "build panicked: {}",
            panic_message(p.as_ref())
        ))
    })
}

/// Join a package thread, turning a panic into a per-package failure so one
/// crashed build does not take the whole run down.
fn join_package(handle: std::thread::JoinHandle<PipelineResult>, rpm_name: &str) -> PipelineResult {
    handle.join().unwrap_or_else(|p| PipelineResult::Failed {
        rpm_name: rpm_name.to_string(),
        reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
        kind: FailureKind::Other,
    })
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
        PipelineResult::Failed { reason, .. } => PackageOutcome::Failed {
            reason: reason.clone(),
        },
    }
}

/// Lists what this run built, what is queued and what the project already has
/// built for this distro and arch, so the outcome is readable without
/// scrolling the log.
fn print_run_summary(memory: &MemorySink, project: &str, distro: &str, arch: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    // Latest outcome per package, so a package that later built is not also
    // reported as queued, and a package listed twice is listed once.
    let mut latest: std::collections::BTreeMap<String, (String, PackageOutcome)> =
        Default::default();
    for ev in memory.finished() {
        if let Event::PackageFinished {
            pkg,
            version,
            outcome,
        } = ev
        {
            latest.insert(pkg, (version, outcome));
        }
    }

    let built: Vec<&String> = latest
        .iter()
        .filter(|(_, (_, outcome))| matches!(outcome, PackageOutcome::Built))
        .map(|(pkg, _)| pkg)
        .collect();
    let _ = writeln!(out, "\nBuilt this run ({}):", built.len());
    for pkg in &built {
        let _ = writeln!(out, "  {pkg} {}", latest[*pkg].0);
    }

    let mut queued: Vec<(&String, Vec<String>)> = latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::MissingDeps { deps } => {
                let mut deps = deps.clone();
                deps.sort();
                deps.dedup();
                Some((pkg, deps))
            }
            _ => None,
        })
        .collect();
    queued.retain(|(pkg, _)| !built.contains(pkg));
    let _ = writeln!(out, "\nQueued, waiting on dependencies ({}):", queued.len());
    for (pkg, deps) in queued {
        if deps.is_empty() {
            let _ = writeln!(out, "  {pkg}");
        } else {
            let _ = writeln!(out, "  {pkg} <- {}", deps.join(", "));
        }
    }

    // OBS reports names in lower case, and this run's packages are already in
    // the built list above, so they are not repeated here.
    let statuses = ap_build::osc::get_project_build_statuses(
        project,
        distro,
        arch,
        ap_build::proc::Emit::quiet(),
    );
    let built_lower: Vec<String> = built.iter().map(|p| p.to_lowercase()).collect();
    let mut cached: Vec<&String> = statuses
        .iter()
        .filter(|(name, s)| {
            matches!(s, ap_build::osc::BuildStatus::Succeeded)
                && !built_lower.contains(&name.to_lowercase())
        })
        .map(|(name, _)| name)
        .collect();
    cached.sort();
    let _ = writeln!(out, "\nAlready built in {project} ({}):", cached.len());
    for name in cached {
        let _ = writeln!(out, "  {name}");
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn cmd_build(
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
    // the `--sweep` doc on `Commands::Build` for why the Oct 6 review
    // flipped this from always-on to opt-in.
    sweep: bool,
    mut source_repo: Option<String>,
    source_urls: Arc<ap_build::sources::SourceMap>,
    python_flavors: Arc<ap_build::flavors::PythonFlavors>,
    run_dir: &RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
    base_reporter: Arc<dyn Reporter + Send + Sync>,
    tui_ctrl: Option<tui::TuiController>,
) -> color_eyre::Result<()> {
    // Reporter for events belonging to a single package: still delivered to
    // jsonl/memory like `reporter`, but tagged so the TUI can attribute
    // per-step events to the right row (see `tui::TuiController::tap`).
    //
    // `PkgContextReporter` sets an ambient pkg name for the duration of each
    // `report()` call so JsonlSink (and any other reporter that cares) can
    // tag its output line with the package name — otherwise JSONL step
    // events from two parallel package threads would be indistinguishable
    // (same `StepId(0)` for both). The 2026-10-06 external review flagged
    // this gap: the TUI already had attribution via `tap`, but the base
    // JSONL stream had none.
    let pkg_reporter = |pkg: &str| -> Arc<dyn Reporter + Send + Sync> {
        let tagged: Arc<dyn Reporter + Send + Sync> = Arc::new(
            ap_core::sink::PkgContextReporter::new(base_reporter.clone(), Some(pkg.to_string())),
        );
        match &tui_ctrl {
            Some(t) => t.tap(tagged, Some(pkg.to_string())),
            None => tagged,
        }
    };
    // Resolve the version if it was not pinned. A repository has no package
    // index behind it, so "latest" comes from the forge's own release list.
    let pinned_version = version.map(str::to_string);
    let version = match version {
        Some(v) => v.to_string(),
        None => {
            if let Some(repo) = &source_repo {
                let v = ap_build::sources::latest_release(repo)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!("{repo} latest release is {v}"),
                });
                v
            } else {
                // Probe PyPI first — the overwhelmingly common case — then
                // fall back to public-source-repository discovery for names
                // PyPI does not carry (standalone Rust crates, plain C/C++
                // libraries, or PyPI releases that ship no usable sdist).
                // Mirrors `resolve_dependency_config` so a bare CLI name
                // goes through the same resolution a runtime-discovered
                // dependency does, instead of hard-failing the moment PyPI
                // comes back empty.
                // Version-only probe, not fetch_latest_release: a wheel-only
                // release (common for vendor SDKs) has a legitimate
                // info.version but no sdist, and the full parser would reject
                // it before the repo-fallback path even got a chance to run.
                let pypi_version = ap_ecosystem::pypi::fetch_latest_version(name).ok();
                if let Some(v) = pypi_version {
                    v
                } else {
                    let repos = ap_build::pipeline::find_source_repo_candidates(name);
                    let Some(repo_url) = repos.verified.first() else {
                        return Err(color_eyre::eyre::eyre!(
                            "{name} has no usable PyPI release and no \
                             discoverable public source repository. Pass \
                             --url, --source-url, or a repository URL as \
                             the positional argument to point it at the \
                             source."
                        ));
                    };
                    let v = ap_build::sources::latest_release(repo_url)
                        .map_err(|e| color_eyre::eyre::eyre!(e))?;
                    reporter.report(&Event::Progress {
                        step: None,
                        message: format!(
                            "{name}: not on PyPI, using discovered source \
                             repository {repo_url} (latest release {v})"
                        ),
                    });
                    source_repo = Some(repo_url.clone());
                    v
                }
            }
        }
    };

    // A repository tells us two things we were only guessing at: whether it is
    // a Python project or a Rust crate, and what it actually calls itself. Ask
    // it before anything is named after the guess.
    let (name, rpm_name, subprojects) = match &source_repo {
        None => (name.to_string(), ap_build::osc::rpm_name(name), Vec::new()),
        Some(repo) => {
            let probe = ap_build::sources::probe(repo, &version, name)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            // For monorepos the user-provided name IS the source package name;
            // the detected project_name is just one subproject and must not
            // override it. Only apply the detected name for single-project repos.
            let effective_name = if probe.subprojects.len() > 1 {
                name.to_string()
            } else {
                let declared = probe
                    .project_name
                    .clone()
                    .unwrap_or_else(|| name.to_string());
                if !declared.eq_ignore_ascii_case(name) {
                    reporter.report(&Event::Progress {
                        step: None,
                        message: format!(
                            "{repo} calls itself '{declared}', not '{name}' — using that"
                        ),
                    });
                }
                declared
            };
            if probe.subprojects.len() > 1 {
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "{repo} holds {} projects: {}",
                        probe.subprojects.len(),
                        probe
                            .subprojects
                            .iter()
                            .map(|(d, _)| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            let rpm = probe.ecosystem.rpm_name(&effective_name);
            (effective_name, rpm, probe.subprojects)
        }
    };
    let name = name.as_str();

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
        agent_backend: AgentBackend::Cli,
        no_agent,
        force,
        regen,
        source_repo: source_repo.clone(),
        subprojects,
        run_id: run_dir.run_id.clone().0,
        source_urls,
        python_flavors,
        failed_deps: Vec::new(),
        unbuildable_deps: Vec::new(),
    };

    let mut config = config;

    // Deps discovered at runtime that need to be built before the next resolve.
    let mut pending_deps: Vec<String> = Vec::new();
    // Tracks deps we have already attempted to avoid infinite retry loops.
    let mut attempted_deps: std::collections::HashSet<String> = std::collections::HashSet::new();
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
    // OFF by default (Oct 6 review finding #5): the implicit always-on
    // sweep broadened a build request beyond its own dependency closure
    // and quietly resolved unrelated stuck packages to current upstream
    // versions. The user opts in with `--sweep` when they want that; the
    // default build now stays scoped to what the requested target
    // actually needs.
    //
    // `--target` asks for exactly one node and nothing else — see its help
    // text ("Build this specific dependency node instead of the
    // auto-selected one") — so skip the sweep when it's set even if the
    // flag was passed; a `--target` run meant to isolate and debug one
    // package should not pick up unrelated project-wide work.
    if sweep && explicit_target.is_none() {
        // The root target itself must never enter this generic sweep: it
        // goes on to `resolve_dependency_config` just like any other
        // runtime-discovered dependency, which always re-fetches the
        // *latest* PyPI release and clears `source_repo` (see its doc
        // comment) — silently discarding this exact invocation's
        // `--version`/`--source-url`/`--url` and spawning a second,
        // wrongly-configured `run_package` thread racing the correctly
        // configured one `select_target` selects below, against the same
        // checkout and rpm name. OBS showing the root package itself as
        // Failed/Unresolvable/Broken from an earlier interrupted run is
        // exactly the common case this would trigger on.
        let root_key = ap_build::osc::fuzzy_key(&rpm_name);
        let already_stuck: Vec<String> = ap_build::osc::get_project_build_statuses(
            project,
            distro,
            arch,
            ap_build::proc::Emit::run(reporter.as_ref()),
        )
        .into_iter()
        .filter(|(_, status)| {
            matches!(
                status,
                ap_build::osc::BuildStatus::Unresolvable
                    | ap_build::osc::BuildStatus::Failed
                    | ap_build::osc::BuildStatus::Broken
            )
        })
        .map(|(pkg, _)| pkg)
        .filter(|pkg| ap_build::osc::fuzzy_key(pkg) != root_key)
        .collect();
        if !already_stuck.is_empty() {
            ap_core::diag!(
                "{} package(s) already unresolvable/failed/broken in {project}, queuing: {}",
                already_stuck.len(),
                already_stuck.join(", ")
            );
        }
        // Keyed by `fuzzy_key`, not the raw stripped name: OBS's real listing
        // and whatever spelling an agent later reports the same dependency
        // under (`pydantic_core` vs. the real `pydantic-core`, say) are not
        // guaranteed to agree on separators — see `osc::fuzzy_key`'s doc
        // comment. An exact-string seed set would silently fail to recognize
        // its own entries.
        stuck_seeds.extend(
            already_stuck
                .iter()
                .map(|pkg| ap_build::osc::fuzzy_key(pkg.strip_prefix("python-").unwrap_or(pkg))),
        );
        pending_deps.extend(already_stuck);
    }

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
    const MAX_RATE_LIMITED_ROUNDS: usize = 200;
    let mut permanently_failed: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Resolve closure, select all buildable targets, build them in parallel,
    // then repeat until the entire closure is built (or an error occurs).
    loop {
        // Before re-resolving, build any runtime-discovered deps (e.g. test-only
        // deps the agent found to be unresolvable).
        while !pending_deps.is_empty() {
            let batch = std::mem::take(&mut pending_deps);
            let mut newly_unbuildable: Vec<String> = Vec::new();
            let handles: Vec<_> = batch
                .into_iter()
                // Convert RPM names to PyPI names: "python-foo" → "foo"
                // The agent returns RPM names, but we need PyPI names for querying.
                .map(|rpm_name| {
                    rpm_name
                        .strip_prefix("python-")
                        .unwrap_or(&rpm_name)
                        .to_string()
                })
                .filter(|dep| {
                    // A dependency we already built here is available, even while
                    // OBS has not indexed it yet. Its version is logged so a bound
                    // it does not meet is still visible.
                    match built_locally(state_root, &ap_build::osc::rpm_name(dep)) {
                        Some(version) => {
                            ap_core::diag!(
                                "dep queue: dropping {dep} (already built locally at {version})"
                            );
                            false
                        }
                        None => true,
                    }
                })
                .filter(|dep| {
                    // Some PyPI packages (`ninja`, reported by an agent per
                    // convention 27 the same as any other missing
                    // dependency) just redistribute a tool openSUSE already
                    // ships as its own system package of the same name.
                    // Building a `python-<name>` RPM for one would only
                    // duplicate it — see `is_system_tool_alias`.
                    let keep = !ap_build::pipeline::is_system_tool_alias(dep);
                    if !keep {
                        ap_core::diag!(
                            "dep queue: dropping {dep} (redistributes a \
                             system package of the same name — see \
                             SYSTEM_TOOL_ALIASES)"
                        );
                    }
                    keep
                })
                .filter(|dep| {
                    // Must run before the `distro_provides` filter below,
                    // not after: that filter can drop a dep for a reason
                    // unrelated to "already attempted" (it's satisfied by
                    // the distro, not built by us), and the no-progress
                    // detection in the batch loop above (`new_deps`) relies
                    // on every dep that ever passes through here ending up
                    // in `attempted_deps` — regardless of which later filter
                    // drops it — so a package that keeps re-reporting the
                    // same distro-satisfied name is still recognized as
                    // making no progress instead of retrying forever.
                    let fresh = attempted_deps.insert(ap_build::osc::fuzzy_key(dep));
                    if !fresh {
                        ap_core::diag!(
                            "dep queue: dropping {dep} (already attempted \
                             this run — see earlier log lines for the \
                             outcome)"
                        );
                    }
                    fresh
                })
                .filter(|dep| {
                    // A C library's openSUSE package name routinely embeds
                    // its SONAME version (`libpng` -> `libpng16`), which the
                    // own/linked-project checks below can never match by
                    // name — they only compare source package name strings.
                    // `distro_provides` checks the distro's actual
                    // `Provides:` capabilities instead, so a dependency the
                    // distro has shipped for years (observed: `libpng`,
                    // `gcc`, `openssl` all reported "unresolvable" by an
                    // agent that should have known better, but `libpng`
                    // alone slipped past every other filter and got queued
                    // for a redundant from-source build) is recognized
                    // before falling through to "search GitHub and build it
                    // from source".
                    let keep = !ap_build::osc::distro_provides(dep);
                    if !keep {
                        ap_core::diag!(
                            "dep queue: dropping {dep} (the distro already \
                             provides it — see `zypper search --provides \
                             --match-exact {dep}`)"
                        );
                    }
                    keep
                })
                .filter(|dep| {
                    // Skip deps already resolvable in the project or any of
                    // its configured build/path projects — unless the
                    // package is in OUR project but in a non-resolvable
                    // state (Failed/Unresolvable/Broken/Disabled/Excluded/
                    // Unknown/missing-status), which is exactly the case
                    // this queue exists to retry.
                    //
                    // This used to be a pure "name present in the union of
                    // listed packages" check, which the Oct 6 review's
                    // finding #1 called out for `select_target` and the
                    // user re-reported for this mini-loop: a source entry
                    // in the own project with status Disabled or no entry
                    // at all for the requested distro/arch counted as
                    // "present → resolved", silently dropping the dep from
                    // the queue even though no built binary existed. The
                    // UnresolvableDeps path from `pipeline.rs` reaches
                    // here, so dropping a dep here means the whole
                    // UnresolvableDeps queue appears to do nothing.
                    //
                    // Linked (upstream) project packages are still trusted
                    // on mere presence — we don't own or rebuild those and
                    // their per-arch build status is unreliable to query
                    // from a project we don't control (see
                    // `select_target`'s `linked_packages` comment).
                    //
                    // The ecosystem (and so the real RPM name) is not
                    // known yet at this point — a system package like
                    // `cmake` or `cuda-devel` never gets the `python-`
                    // prefix a Python dependency would, so check both
                    // spellings rather than assuming Python. Everything
                    // here compares by `fuzzy_key`, not the raw string.
                    let bare_key = ap_build::osc::fuzzy_key(dep);
                    if stuck_seeds.contains(&bare_key) {
                        return true;
                    }
                    let rpm_key = ap_build::osc::fuzzy_key(&ap_build::osc::rpm_name(dep));
                    let emit = ap_build::proc::Emit::run(reporter.as_ref());

                    // Own-project packages: index by fuzzy key so a
                    // separator mismatch can't false-miss.
                    // A failed listing is already warned about by osc.
                    let own =
                        ap_build::osc::list_project_packages(project, emit).unwrap_or_default();
                    let own_match = own.iter().find(|p| {
                        let k = ap_build::osc::fuzzy_key(p);
                        k == rpm_key || k == bare_key
                    });
                    if let Some(real_pkg) = own_match {
                        // Package exists in our project. Only skip queuing
                        // it when its OBS build status is actually
                        // resolvable — i.e. there's a built binary (or one
                        // genuinely in flight). An Unresolvable/Failed/
                        // Broken/Disabled/Excluded/Unknown or missing-
                        // status entry stays queued so the agent can look
                        // at it.
                        let statuses =
                            ap_build::osc::get_project_build_statuses(project, distro, arch, emit);
                        let status = statuses.get(real_pkg);
                        let skip = ap_build::pipeline::status_is_resolvable(status);
                        if skip {
                            ap_core::diag!(
                                "dep queue: dropping {dep} (already in \
                                 {project} as {real_pkg}, status {:?} — \
                                 treating as resolvable)",
                                status.map(|s| s.to_string()).unwrap_or_default()
                            );
                        } else {
                            ap_core::diag!(
                                "dep queue: keeping {dep} (in {project} as \
                                 {real_pkg} with status {:?} — not \
                                 resolvable, retrying)",
                                status.map(|s| s.to_string()).unwrap_or_default()
                            );
                        }
                        return !skip;
                    }

                    // Linked / build-path projects: trust presence (see
                    // comment above).
                    for (path_project, _repo) in
                        ap_build::osc::get_project_repo_paths(project, emit)
                    {
                        let linked = ap_build::osc::list_project_packages(&path_project, emit)
                            .unwrap_or_default();
                        if let Some(match_name) = linked.iter().find(|p| {
                            let k = ap_build::osc::fuzzy_key(p);
                            k == rpm_key || k == bare_key
                        }) {
                            ap_core::diag!(
                                "dep queue: dropping {dep} (available from \
                                 linked project {path_project} as {match_name})"
                            );
                            return false;
                        }
                    }
                    // A local workspace already existing for this name means
                    // some earlier round already checked it out — most
                    // likely it's a node in the target's own pip-resolution
                    // closure, mid-build or queued for the next round via
                    // the ordinary frontier mechanism (`select_target`),
                    // which knows nothing about this ad-hoc queue and will
                    // keep retrying it on its own. A second, redundant
                    // report of the same dependency here (a real case: one
                    // package's agent reporting `z3-solver` as missing while
                    // `python-z3-solver` is simultaneously mid-build
                    // elsewhere in the very same run — OBS genuinely didn't
                    // have it *yet*, but "not yet" isn't "nobody's on it")
                    // must not spawn a second, parallel build attempt. This
                    // check comes after the `stuck_seeds` early-return above,
                    // not before: a genuinely stuck package also has local
                    // state, and that's exactly the case this queue exists
                    // to retry.
                    let state_path = config
                        .state_root
                        .join(ap_core::run::sanitize_name(&ap_build::osc::rpm_name(dep)))
                        .join("state.json");
                    let keep = !state_path.exists();
                    if !keep {
                        ap_core::diag!(
                            "dep queue: dropping {dep} (local workspace at \
                             {} already exists — probably mid-build via the \
                             frontier mechanism; it keeps retrying on its own)",
                            state_path.display()
                        );
                    }
                    keep
                })
                .filter_map(|dep| {
                    let dep_config =
                        match resolve_dependency_config(&dep, &config, reporter.as_ref()) {
                            Some(c) => c,
                            None => {
                                // No PyPI project and no discoverable source repo —
                                // this is never going to become buildable no matter
                                // how many times it's reported. Record it so every
                                // package needing it learns that from its next
                                // prompt (convention 28 Exception 4) instead of
                                // reporting it again forever.
                                newly_unbuildable.push(dep);
                                return None;
                            }
                        };
                    let dep_rpm = dep_config.rpm_name.clone();
                    let dep_pkg_run = run_dir.pkg(&dep_rpm);
                    if dep_pkg_run.ensure().is_err() {
                        return None;
                    }
                    reporter.report(&Event::TargetSelected {
                        pkg: dep_rpm.clone(),
                        version: dep_config.version.clone(),
                        reason: SelectionReason::FirstUnbuiltDependency,
                        blocked_by: vec![],
                    });
                    let dep_version = dep_config.version.clone();
                    let r = pkg_reporter(&dep_rpm);
                    let sem2 = sem.clone();
                    let finished_name = dep_rpm.clone();
                    let finished_version = dep_version.clone();
                    let handle = std::thread::spawn(move || {
                        let _permit = Permit::acquire(&sem2);
                        let result = run_package(&dep_config, &dep_pkg_run, r.as_ref());
                        // Reported as soon as this package is done. Reporting in
                        // join order let a slower sibling hold back every package
                        // finished after it, so their rows stayed on their last step.
                        r.report(&Event::PackageFinished {
                            pkg: finished_name,
                            version: finished_version,
                            outcome: package_outcome(&result),
                        });
                        result
                    });
                    Some((dep_rpm, dep_version, handle))
                })
                .collect();

            for dep in newly_unbuildable {
                if !config.unbuildable_deps.contains(&dep) {
                    config.unbuildable_deps.push(dep);
                }
            }

            for (dep_rpm, _version, handle) in handles {
                let result = join_package(handle, &dep_rpm);
                match result {
                    PipelineResult::Built { .. } => {
                        ap_core::diag!("built dep {dep_rpm}");
                    }
                    PipelineResult::AlreadyBuilt { .. } => {
                        ap_core::diag!("dep {dep_rpm} already built");
                    }
                    PipelineResult::MissingDeps { deps: more, .. } => {
                        pending_deps.extend(more);
                    }
                    PipelineResult::Failed { reason, .. } => {
                        pkg_reporter(&dep_rpm).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "dependency {dep_rpm} failed to build ({reason}); \
                                 retrying the package that needs it"
                            ),
                        });
                        // Record it so the dependent package's agent can see
                        // that this one was attempted — without that evidence
                        // it can only report the dep again and stall.
                        if !config.failed_deps.contains(&dep_rpm) {
                            config.failed_deps.push(dep_rpm);
                        }
                    }
                }
            }
        }

        // A repository target is a single package: there is no PyPI closure to
        // walk, and pip could not resolve the name even if we asked it to.
        // Missing deps surface as MissingDeps; we build them via the PyPI path
        // in the pending_deps loop above, then retry this package.
        if source_repo.is_some() {
            let pkg_run = run_dir.pkg(&rpm_name);
            pkg_run.ensure()?;
            reporter.report(&Event::TargetSelected {
                pkg: rpm_name.clone(),
                version: version.clone(),
                reason: SelectionReason::ExplicitOverride,
                blocked_by: vec![],
            });
            let r = pkg_reporter(&rpm_name);
            let result = run_package(&config, &pkg_run, r.as_ref());
            let outcome = match &result {
                PipelineResult::Built { .. } => PackageOutcome::Built,
                PipelineResult::AlreadyBuilt { .. } => PackageOutcome::AlreadyBuilt,
                PipelineResult::MissingDeps { deps, .. } => {
                    PackageOutcome::MissingDeps { deps: deps.clone() }
                }
                PipelineResult::Failed { reason, .. } => PackageOutcome::Failed {
                    reason: reason.clone(),
                },
            };
            r.report(&Event::PackageFinished {
                pkg: rpm_name.clone(),
                version: version.clone(),
                outcome,
            });
            match result {
                PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => return Ok(()),
                PipelineResult::MissingDeps { deps, .. } => {
                    // Only queue deps we haven't tried yet; if every dep was
                    // already attempted we cannot make progress. Compared by
                    // `fuzzy_key`, not the raw string — see its doc comment.
                    // A dep attempted earlier may have been built locally since;
                    // it is available now, so drop it before judging progress.
                    let remaining: Vec<String> = deps
                        .iter()
                        .filter(|d| {
                            built_locally(&config.state_root, &ap_build::osc::rpm_name(d)).is_none()
                        })
                        .cloned()
                        .collect();
                    let new_deps: Vec<String> = remaining
                        .iter()
                        .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
                        .cloned()
                        .collect();
                    if new_deps.is_empty() && !remaining.is_empty() {
                        // Previously fell through to `return Ok(())` here,
                        // which the drogon-shaped case (keeps surfacing
                        // the SAME unresolvable deps because the agent
                        // can't rename them to anything findable on a
                        // forge) silently exited successful on. A target
                        // that still needs deps was NOT built — surface
                        // that as a failure, same shape as the frontier
                        // path's no-progress give-up.
                        let unbuildable = config.unbuildable_deps.join(", ");
                        let failed = config.failed_deps.join(", ");
                        return Err(color_eyre::eyre::eyre!(
                            "{rpm_name} needs {} but every one has already \
                             been attempted without success{}{}; cannot \
                             build. Check the per-dep warnings above for \
                             WHY each dep couldn't be queued (no PyPI \
                             release AND no discoverable source repo is \
                             the usual answer — pass --source-url to \
                             point it at one).",
                            deps.join(", "),
                            if unbuildable.is_empty() {
                                String::new()
                            } else {
                                format!(" (unbuildable: {unbuildable})")
                            },
                            if failed.is_empty() {
                                String::new()
                            } else {
                                format!(" (failed-to-build: {failed})")
                            },
                        ));
                    }
                    pending_deps.extend(new_deps);
                    continue;
                }
                PipelineResult::Failed { reason, .. } => {
                    return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
                }
            }
        }

        // Select all currently buildable targets (the full frontier).
        //
        // `config.force` is passed down so `--force` (and `update`, which
        // always sets it) can override the "OBS says it's already green"
        // resolvability gate for the root -- see `select_target`'s doc
        // comment. Without this, the gate would skip the package before
        // `run_package`'s own force check could ever fire.
        let targets = select_target(
            name,
            explicit_target,
            pinned_version.as_deref(),
            &config,
            &permanently_failed,
            config.force,
            reporter.as_ref(),
        )
        .map_err(|e| color_eyre::eyre::eyre!(e))?;

        if targets.is_empty() {
            if permanently_failed.is_empty() {
                ap_core::diag!("{name} and all dependencies are already built");
                return Ok(());
            }
            let mut names: Vec<&String> = permanently_failed.iter().collect();
            names.sort();
            let names = names.into_iter().cloned().collect::<Vec<_>>().join(", ");
            return Err(color_eyre::eyre::eyre!(
                "build incomplete — gave up on {} after {max_target_retries} failed attempt(s) \
                 each, blocking everything left in the closure that needs them: {names}",
                permanently_failed.len()
            ));
        }

        for t in &targets {
            reporter.report(&Event::TargetSelected {
                pkg: ap_build::osc::rpm_name(&t.name),
                version: t.version.clone(),
                reason: t.reason.clone(),
                blocked_by: t.blocked_by.clone(),
            });
        }

        // Spawn one thread per frontier package; OBS builds run concurrently.
        let mut handles: Vec<(String, String, std::thread::JoinHandle<PipelineResult>)> =
            Vec::new();
        let mut spawn_error: Option<color_eyre::Report> = None;
        for target_info in targets {
            let rpm_name = ap_build::osc::rpm_name(&target_info.name);
            let pkg_config = PipelineConfig {
                pkg_name: target_info.name.clone(),
                rpm_name: rpm_name.clone(),
                version: target_info.version.clone(),
                ..config.clone()
            };
            let pkg_run = run_dir.pkg(&rpm_name);
            if let Err(e) = pkg_run.ensure() {
                spawn_error = Some(color_eyre::eyre::eyre!(
                    "cannot create the package run directory {}: {e}",
                    pkg_run.path.display()
                ));
                break;
            }
            let r = pkg_reporter(&rpm_name);
            let sem2 = sem.clone();
            let finished_name = rpm_name.clone();
            let finished_version = target_info.version.clone();
            let handle = std::thread::spawn(move || {
                let _permit = Permit::acquire(&sem2);
                let result = run_package(&pkg_config, &pkg_run, r.as_ref());
                // Reported as soon as this package is done. Reporting in join
                // order let a slower sibling hold back every package finished
                // after it, so their rows stayed on their last step.
                r.report(&Event::PackageFinished {
                    pkg: finished_name,
                    version: finished_version,
                    outcome: package_outcome(&result),
                });
                result
            });
            handles.push((rpm_name, target_info.version, handle));
        }
        if let Some(e) = spawn_error {
            // Join the workers already spawned so none is left running
            // unobserved once we return.
            for (rpm_name, _version, handle) in handles {
                join_package(handle, &rpm_name);
            }
            return Err(e);
        }

        let mut batch_pending: Vec<String> = Vec::new();
        let mut batch_failures: Vec<(String, String)> = Vec::new();

        for (rpm_name, version, handle) in handles {
            let result = join_package(handle, &rpm_name);
            match result {
                PipelineResult::Built { .. } => {
                    ap_core::diag!("built {rpm_name} {version}");
                    fail_counts.remove(&rpm_name);
                }
                PipelineResult::AlreadyBuilt { .. } => {
                    ap_core::diag!("{rpm_name} {version} already built");
                    fail_counts.remove(&rpm_name);
                }
                PipelineResult::MissingDeps { deps, .. } => {
                    // No-progress detection (review finding #15 in the Sep
                    // report; still open in Oct). A package that keeps
                    // surfacing MissingDeps on names already in
                    // `attempted_deps` — and therefore skipped by the
                    // mini-loop every time — is not making progress; it is
                    // blocked on something that can never be built in this
                    // environment (closed-source toolchain, PyPI name that
                    // doesn't exist, …). Without this, the frontier would
                    // re-select the same package forever: MissingDeps does
                    // not consume the outer failed-target limit, so the
                    // loop never gives up on it.
                    //
                    // Treat such a no-progress round the same as a Failed
                    // result for retry-counting purposes: increment
                    // `fail_counts`, and after `max_target_retries` of
                    // genuinely-no-new-deps rounds, mark the package
                    // permanently_failed and let the frontier move on.
                    //
                    // A dep attempted earlier may have been built locally since
                    // (by a sibling in this round, say); it is available now, so
                    // it counts as progress, not as an exhausted dependency.
                    let remaining: Vec<&String> = deps
                        .iter()
                        .filter(|d| {
                            built_locally(&config.state_root, &ap_build::osc::rpm_name(d)).is_none()
                        })
                        .collect();
                    let new_deps: Vec<&&String> = remaining
                        .iter()
                        .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
                        .collect();
                    if new_deps.is_empty() && !remaining.is_empty() {
                        // Repeating this package cannot help: every dependency it
                        // names was already attempted and dropped by the dep queue,
                        // and attempted names are never queued again. Give up on the
                        // first such round instead of re-running the pipeline up to
                        // `max_target_retries` times at real agent cost.
                        let count = fail_counts.entry(rpm_name.clone()).or_insert(0);
                        *count += 1;
                        permanently_failed.insert(rpm_name.clone());
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "giving up on {rpm_name}: it needs {} and each of those was \
                                 already attempted and dropped (see the dep queue warnings \
                                 above for why); anything depending on it stays blocked",
                                deps.join(", ")
                            ),
                        });
                    }
                    batch_pending.extend(deps);
                }
                PipelineResult::Failed { reason, kind, .. } => {
                    batch_failures.push((rpm_name.clone(), reason.clone()));
                    if kind == FailureKind::Busy {
                        // Another process is building this package, so nothing
                        // was tried here. It is deferred to a later round and
                        // does not count toward giving up.
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "{rpm_name} is being built by another process ({reason}); \
                                 deferred to a later round"
                            ),
                        });
                        continue;
                    }
                    let rate_limited = kind == FailureKind::RateLimited;
                    // A package that already spent its *entire* budget or attempt
                    // allowance on this round gives up at once, instead of
                    // getting a fresh budget for each of `max_target_retries`
                    // rounds. `max_target_retries` reuses `max_attempts` as the
                    // whole-round retry count, so without this a package that
                    // burned its budget would repeat the same loop over and over.
                    let exhausted = kind == FailureKind::Exhausted;
                    let count = if rate_limited {
                        rate_limited_counts.entry(rpm_name.clone()).or_insert(0)
                    } else {
                        fail_counts.entry(rpm_name.clone()).or_insert(0)
                    };
                    *count += 1;
                    let limit = if rate_limited {
                        MAX_RATE_LIMITED_ROUNDS
                    } else {
                        max_target_retries
                    };
                    if exhausted || *count >= limit {
                        permanently_failed.insert(rpm_name.clone());
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: if exhausted {
                                format!(
                                    "giving up on {rpm_name}: it already spent its full \
                                     budget/attempt allowance this round without succeeding \
                                     ({reason}) — another round would just repeat the same \
                                     fix loop with a fresh budget; anything depending on it \
                                     stays blocked, but the rest of the closure keeps going"
                                )
                            } else {
                                format!(
                                    "giving up on {rpm_name} after {count} failed attempts \
                                     ({reason}); anything depending on it stays blocked, but \
                                     the rest of the closure keeps going"
                                )
                            },
                        });
                    } else {
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "{rpm_name} failed (attempt {count}/{limit}): \
                                 {reason}; retrying next round"
                            ),
                        });
                    }
                }
            }
        }

        // Compute what, if anything, from `batch_pending` is actually new
        // information *before* merging it into `pending_deps` — the
        // explicit-target branch below needs to decide "did this round
        // discover any new missing deps, or are we spinning on the same ones
        // every pass" and the merged list hides that.
        let new_pending: Vec<String> = batch_pending
            .iter()
            .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
            .cloned()
            .collect();
        pending_deps.extend(batch_pending);

        // If an explicit target was requested, stop after one batch — there is
        // no "next round" to retry it in, so a failure here must surface as
        // an error rather than a silent `Ok`.
        if explicit_target.is_some() {
            if let Some((rpm_name, reason)) = batch_failures.first() {
                return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
            }
            // A `--target` run returning `MissingDeps` previously fell through
            // to `None => Ok(())` here, reporting success for a target that
            // was *not* built. The Oct 6 review reproduced this: an explicit
            // target whose first batch yielded `MissingDeps` exited 0 with
            // nothing green. Make unresolved work a non-success outcome: if
            // this batch surfaced genuinely new deps (not ones we have
            // already tried and failed/gave-up on), loop around once to let
            // them build via the top-of-loop pending-deps processor and
            // re-select the target; if we would loop on the same deps
            // forever, error out instead.
            if !pending_deps.is_empty() && new_pending.is_empty() {
                let missing: Vec<&String> = pending_deps.iter().collect();
                let names = missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(color_eyre::eyre::eyre!(
                    "{name} needs {} but every one has already been attempted \
                     without success; cannot build",
                    names
                ));
            }
            if pending_deps.is_empty() {
                return Ok(());
            }
            // Fall through to the next loop iteration so `pending_deps` drains
            // (building each new dep), then `select_target` re-selects the
            // same explicit target and `run_package` retries with those deps
            // now available.
        }
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
/// starting over. The Oct 6 review flagged the previous always-regenerate
/// default as silently discarding recipe-specific work.
///
/// Rebuilds run sequentially by default (`--jobs 1`) because each one already
/// kicks off its own dep-closure concurrency inside `cmd_build`; stacking
/// multiple top-level updates on top of that would multiply, not add. A user
/// who knows their machine can take it can raise `--jobs`.
#[allow(clippy::too_many_arguments)]
fn cmd_update(
    project: &str,
    only: Option<&str>,
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
    let emit = ap_build::proc::Emit::run(reporter.as_ref());
    let candidates = ap_build::update::plan(project, only, emit);

    if candidates.is_empty() {
        reporter.report(&Event::Progress {
            step: None,
            message: match only {
                Some(name) => format!("no package named {name} in {project}"),
                None => format!("project {project} has no packages"),
            },
        });
        return Ok(());
    }

    report_update_plan(&candidates, reporter.as_ref());

    if check {
        return Ok(());
    }

    let to_build: Vec<(ap_build::update::Ecosystem, String)> = candidates
        .iter()
        .filter_map(|c| match &c.decision {
            ap_build::update::Decision::Update {
                ecosystem,
                new_version,
                ..
            } => Some((ecosystem.clone(), new_version.clone())),
            _ => None,
        })
        .collect();

    if to_build.is_empty() {
        reporter.report(&Event::Progress {
            step: None,
            message: "nothing to update".into(),
        });
        return Ok(());
    }

    let mut failures: Vec<(String, String)> = Vec::new();
    for (ecosystem, new_version) in to_build {
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
        );
        if let Err(e) = r {
            failures.push((name, e.to_string()));
        }
    }

    match failures.first() {
        Some((pkg, err)) => Err(color_eyre::eyre::eyre!(
            "{} update(s) failed; first: {pkg} ({err})",
            failures.len()
        )),
        None => Ok(()),
    }
}

/// Render the planned updates as per-line Progress events so the human/TUI
/// sinks all see the same formatting without this function knowing which
/// backend is active.
fn report_update_plan(
    candidates: &[ap_build::update::Candidate],
    reporter: &(dyn Reporter + Send + Sync),
) {
    use ap_build::update::{Decision, Ecosystem};
    let eco_tag = |e: &Ecosystem| match e {
        Ecosystem::Pypi { .. } => "pypi",
        Ecosystem::GitRepo { .. } => "git",
    };
    let mut n_update = 0;
    let mut n_uptodate = 0;
    let mut n_skipped = 0;
    for c in candidates {
        let line = match &c.decision {
            Decision::Update {
                ecosystem,
                old_version,
                new_version,
            } => {
                n_update += 1;
                format!(
                    "[update]  {}  {} → {}  ({})",
                    c.rpm_name,
                    old_version,
                    new_version,
                    eco_tag(ecosystem),
                )
            }
            Decision::UpToDate { ecosystem, version } => {
                n_uptodate += 1;
                format!(
                    "[current] {}  {}  ({})",
                    c.rpm_name,
                    version,
                    eco_tag(ecosystem)
                )
            }
            Decision::Skipped(reason) => {
                n_skipped += 1;
                format!("[skip]    {}  ({reason})", c.rpm_name)
            }
        };
        reporter.report(&Event::Progress {
            step: None,
            message: line,
        });
    }
    reporter.report(&Event::Progress {
        step: None,
        message: format!(
            "update plan: {n_update} to update, {n_uptodate} up to date, {n_skipped} skipped"
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_package_name_ignores_subpaths_and_queries() {
        for url in [
            "https://github.com/o/r",
            "https://github.com/o/r.git",
            "https://github.com/o/r/tree/main",
            "https://github.com/o/r/releases/tag/v1",
            "https://github.com/o/r?tab=readme",
            "https://github.com/o/r#readme",
        ] {
            assert_eq!(repo_package_name(url), "r", "{url}");
        }
    }

    #[test]
    fn validate_package_name_rejects_unsafe_names() {
        assert!(validate_package_name("ripgrep").is_ok());
        for bad in ["", "-rf", "a/b", "a b", "a\nb", "a\tb"] {
            assert!(validate_package_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn permit_releases_on_panic() {
        let sem = make_semaphore(1);
        let s2 = sem.clone();
        let r = std::thread::spawn(move || {
            let _p = Permit::acquire(&s2);
            panic!("boom");
        })
        .join();
        assert!(r.is_err());
        // Would deadlock if the slot had leaked.
        let _p = Permit::acquire(&sem);
    }

    /// drogon-shaped case the user reported: the agent passed
    /// `trantor-devel` through as the dep name instead of translating
    /// to upstream `trantor`. The candidate list must strip the
    /// `-devel` suffix so `find_source_repo_candidates` has a chance.
    #[test]
    fn upstream_name_candidates_strips_devel_suffix() {
        assert_eq!(
            upstream_name_candidates("trantor-devel"),
            vec!["trantor-devel".to_string(), "trantor".to_string()]
        );
    }

    #[test]
    fn upstream_name_candidates_strips_lib_prefix_and_devel_suffix_together() {
        // The full RPM BuildRequires shape. Three candidates: original,
        // -devel stripped, both stripped.
        assert_eq!(
            upstream_name_candidates("libtrantor-devel"),
            vec![
                "libtrantor-devel".to_string(),
                "libtrantor".to_string(),
                "trantor".to_string(),
            ]
        );
    }

    #[test]
    fn upstream_name_candidates_handles_hyphenated_project_names() {
        // yaml-cpp is drogon's other recurring BuildRequires. The
        // hyphen inside the name must survive — only the trailing
        // -devel is cruft.
        assert_eq!(
            upstream_name_candidates("yaml-cpp-devel"),
            vec!["yaml-cpp-devel".to_string(), "yaml-cpp".to_string()]
        );
    }

    #[test]
    fn upstream_name_candidates_also_recognises_dev_and_headers() {
        for (dep, stripped) in [
            ("something-dev", "something"),
            ("whatever-headers", "whatever"),
        ] {
            let out = upstream_name_candidates(dep);
            assert!(out.contains(&stripped.to_string()), "{dep:?} → {out:?}");
        }
    }

    #[test]
    fn upstream_name_candidates_leaves_a_canonical_name_alone() {
        // Nothing to strip — only one candidate, same as pre-fix
        // behavior for correctly-translated deps.
        assert_eq!(
            upstream_name_candidates("trantor"),
            vec!["trantor".to_string()]
        );
        assert_eq!(upstream_name_candidates("httpx"), vec!["httpx".to_string()]);
    }

    #[test]
    fn upstream_name_candidates_does_not_strip_lib_from_a_project_whose_name_is_lib() {
        // Pathological: a project called just `lib` would strip to
        // empty. Guard against the empty-string candidate — we skip it,
        // keeping just the original.
        assert_eq!(upstream_name_candidates("lib"), vec!["lib".to_string()]);
    }

    #[test]
    fn a_repo_url_is_recognised_in_the_name_position() {
        assert!(looks_like_url("https://github.com/BurntSushi/ripgrep"));
        assert!(looks_like_url("git@github.com:BurntSushi/ripgrep.git"));
        assert!(!looks_like_url("markupsafe"));
        assert!(!looks_like_url("nvidia-ml-py"));
    }

    #[test]
    fn the_package_name_comes_from_the_last_path_segment() {
        for url in [
            "https://github.com/BurntSushi/ripgrep",
            "https://github.com/BurntSushi/ripgrep/",
            "https://github.com/BurntSushi/ripgrep.git",
            "git@github.com:BurntSushi/ripgrep.git",
        ] {
            assert_eq!(repo_package_name(url), "ripgrep", "{url}");
        }
        assert_eq!(
            repo_package_name("https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz"),
            "hello"
        );
        assert_eq!(
            repo_package_name("https://download.savannah.gnu.org/releases/lzip/"),
            "lzip"
        );
        assert_eq!(
            repo_package_name("https://gitlab.com/group/sub/thing"),
            "thing"
        );
    }
}

#[cfg(test)]
mod built_locally_tests {
    use super::built_locally;

    #[test]
    fn only_a_successful_local_build_counts_as_available() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("python-cuda-bindings");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("state.json"),
            r#"{"last_outcome":"success","last_built_version":"13.4.3"}"#,
        )
        .unwrap();
        assert_eq!(
            built_locally(dir.path(), "python-cuda-bindings").as_deref(),
            Some("13.4.3")
        );
        assert_eq!(built_locally(dir.path(), "python-missing"), None);
        // The guessed spelling must not matter: `_` and `-` are the same project.
        std::fs::create_dir_all(dir.path().join("python-typing_extensions")).unwrap();
        std::fs::write(
            dir.path()
                .join("python-typing_extensions")
                .join("state.json"),
            r#"{"last_outcome":"success","last_built_version":"4.15.0"}"#,
        )
        .unwrap();
        assert_eq!(
            built_locally(dir.path(), "python-typing-extensions").as_deref(),
            Some("4.15.0")
        );
        std::fs::write(
            pkg.join("state.json"),
            r#"{"last_outcome":"missing_deps","last_built_version":"13.4.3"}"#,
        )
        .unwrap();
        assert_eq!(built_locally(dir.path(), "python-cuda-bindings"), None);
    }
}
