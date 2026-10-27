//! Command-line surface — REDESIGN M1.

mod tui;
mod update_project;

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

        /// Resolve dependencies without the sandbox: pip runs on the host and
        /// may build sdists, running their build backends as you. Without
        /// this, pip runs in bubblewrap, or resolves wheels only when
        /// bubblewrap is unusable. Same as AUTO_PACKAGE_UNSAFE_RESOLVE=1.
        #[arg(long, default_value_t = false)]
        unsafe_resolve: bool,

        /// Remove the work directory before building.
        #[arg(long, default_value_t = false)]
        cleanup: bool,

        /// Maximum number of packages to build concurrently (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        jobs: usize,

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
        /// asked for; the Oct 6 review flagged the implicit sweep (then
        /// always-on) as broadening the operation beyond the requested
        /// closure and quietly resolving unrelated packages to current
        /// upstream versions.
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

/// PyPI's latest version of `name`, or `None` when PyPI has no such project.
/// Any other failure (network, 5xx, unusable answer) also yields `None`, so
/// the caller falls back to repository discovery, but is warned about first:
/// "PyPI could not be asked" must not read as "not on PyPI".
fn pypi_latest_version(name: &str, reporter: &dyn Reporter) -> Option<String> {
    match ap_ecosystem::pypi::fetch_latest_version(name) {
        Ok(v) => Some(v),
        Err(e) if e.is_not_found() => None,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("PyPI lookup for {name} failed, treating it as not on PyPI: {e}"),
            });
            None
        }
    }
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
    native: bool,
    config: &PipelineConfig,
    reporter: &dyn Reporter,
) -> Option<PipelineConfig> {
    // A C/C++ library (reported as `lib<x>-devel`, `pkgconfig(x)`, a soname) is
    // never looked up on PyPI: a PyPI project that happens to share its name is
    // not it, and would be packaged as `python-<x>`.
    let parent_is_python = config.rpm_name.starts_with("python-") && !native;

    // `--nvidia-redist`: a CUDA component is NVIDIA's archive at the version of
    // NVIDIA's manifest, whatever PyPI says about the wheel or a repository
    // about the name.
    if let Some(version) = ap_build::redist::latest_version(&ap_build::redist::Network, dep) {
        return match version {
            Ok(version) => {
                let rpm_name = ap_build::osc::rpm_name(dep);
                ap_core::diag!("  queuing missing dependency: {rpm_name} (NVIDIA {version})");
                Some(PipelineConfig {
                    pkg_name: dep.to_string(),
                    rpm_name,
                    version,
                    source_repo: None,
                    subprojects: Vec::new(),
                    // The root package's findings are not a dependency's.
                    patch_request: None,
                    patch_condition: None,
                    ..config.clone()
                })
            }
            Err(e) => {
                reporter.report(&Event::Warning {
                    step: None,
                    message: format!("cannot package {dep} from NVIDIA's archives: {e}"),
                });
                None
            }
        };
    }

    if parent_is_python {
        // Use the version-only path, not fetch_latest_release: a wheel-only
        // release (NVIDIA CUDA toolkit packages, Triton, etc.) has a real
        // info.version but no sdist, and fetch_latest_release would reject
        // it on the sdist check — leaving the dependency unqueued even
        // though PyPI could tell us its version perfectly well.
        let pypi_version = pypi_latest_version(dep, reporter);

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
                // The root package's findings are not a dependency's.
                patch_request: None,
                patch_condition: None,
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
    // A repository's latest release does not depend on the name it was found
    // under, so each is asked for once per run, and the new ones for one
    // candidate are asked for side by side.
    let mut releases: std::collections::HashMap<String, Result<String, String>> =
        std::collections::HashMap::new();
    for cand in &name_candidates {
        let repos = ap_build::pipeline::find_source_repo_candidates(cand);
        let unknown: Vec<String> = repos
            .verified
            .iter()
            .filter(|r| !releases.contains_key(*r))
            .cloned()
            .collect();
        releases.extend(
            unknown
                .iter()
                .cloned()
                .zip(ap_build::sources::latest_releases(&unknown)),
        );
        for repo_url in &repos.verified {
            all_attempts.push(repo_url.clone());
            let version = match &releases[repo_url] {
                Ok(v) => v.clone(),
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
            if ap_build::osc::fuzzy_key(&dep_rpm) == ap_build::osc::fuzzy_key(&config.rpm_name) {
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
                // The root package's findings are not a dependency's.
                patch_request: None,
                patch_condition: None,
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
             --source-url {dep}=<url> to point it at the upstream repo \
             (a repository found only by a name search is not used \
             without that confirmation)."
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
    // Sets the shutdown flag first, so polling loops and the scheduler stop
    // on their next check.
    ap_build::proc::kill_all_groups();
    ap_build::proc::remove_private_dir();
    ap_core::sink::flush_jsonl_sinks();
    if tui_active {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    }
    let usage = memory.usage();
    eprintln!(
        "\nInterrupted — tokens: {} in / {} out — cost: ${:.4}",
        usage.input_tokens, usage.output_tokens, usage.cost_usd
    );
    // The shell convention for death by SIGINT; scripts can tell an
    // interrupted run from a failed one.
    std::process::exit(130);
}

/// Route Ctrl-C (and, with ctrlc's `termination` feature, SIGTERM/SIGHUP) to
/// [`handle_interrupt`].
fn install_interrupt_handler(tui_active: bool, memory: Arc<MemorySink>) -> color_eyre::Result<()> {
    ctrlc::set_handler(move || handle_interrupt(tui_active, &memory))
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

/// Name of the per-project lock file, kept inside the project directory.
const PROJECT_LOCK_NAME: &str = ".auto-package.lock";

/// Refuse a project directory that is a symlink: `create_dir_all` and the
/// marker write would follow it and put state (and `--cleanup`'s deletes)
/// wherever it points, e.g. `~/Documents`.
/// How the NVIDIA CUDA libraries are packaged (`--nvidia`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum NvidiaMode {
    /// The `nvidia-*` PyPI wheels, repackaged as `python-nvidia-*`.
    Wheel,
    /// NVIDIA's redistributable archives, packaged as `cuda-<name>`.
    Redist,
}

impl NvidiaMode {
    fn as_str(self) -> &'static str {
        match self {
            NvidiaMode::Wheel => "wheel",
            NvidiaMode::Redist => "redist",
        }
    }
}

/// Where a work directory remembers its `--nvidia` choice.
const NVIDIA_MODE_FILE: &str = "nvidia-mode";

fn read_nvidia_mode(project_dir: &Path) -> Option<NvidiaMode> {
    match std::fs::read_to_string(project_dir.join(NVIDIA_MODE_FILE))
        .ok()?
        .trim()
    {
        "wheel" => Some(NvidiaMode::Wheel),
        "redist" => Some(NvidiaMode::Redist),
        _ => None,
    }
}

/// The mode to run in, and a warning when it changes what the work directory
/// last used. An explicit choice wins, then the remembered one, then `wheel`.
/// The two modes name their packages differently (`cuda-cublas` against
/// `python-nvidia-cublas`) and never share state, so flipping on a restart
/// without being asked would build a second copy of every CUDA package.
pub(crate) fn resolve_nvidia_mode(
    explicit: Option<NvidiaMode>,
    remembered: Option<NvidiaMode>,
) -> (NvidiaMode, Option<String>) {
    match (explicit, remembered) {
        (Some(chosen), Some(before)) if chosen != before => (
            chosen,
            Some(format!(
                "this work directory was run with --nvidia {}; switching to {}. Packages \
                 already built the other way stay in the project: the modes name them \
                 differently (cuda-<name> against python-nvidia-<name>)",
                before.as_str(),
                chosen.as_str()
            )),
        ),
        (Some(chosen), _) => (chosen, None),
        (None, Some(before)) => (before, None),
        (None, None) => (NvidiaMode::Wheel, None),
    }
}

/// Remember `mode` in the work directory. Called once `--cleanup` has had its
/// chance to empty it. A plain run in the default mode leaves no file behind.
pub(crate) fn write_nvidia_mode(
    project_dir: &Path,
    mode: NvidiaMode,
    explicit: bool,
) -> color_eyre::Result<()> {
    if mode != NvidiaMode::Wheel || explicit || project_dir.join(NVIDIA_MODE_FILE).exists() {
        ap_core::fsx::write_atomic(
            &project_dir.join(NVIDIA_MODE_FILE),
            format!("{}\n", mode.as_str()),
        )?;
    }
    Ok(())
}

/// Decide the mode for this run and switch the pipeline to it.
pub(crate) fn apply_nvidia_mode(
    project_dir: &Path,
    explicit: Option<NvidiaMode>,
    cleanup: bool,
) -> color_eyre::Result<NvidiaMode> {
    let remembered = if cleanup {
        None
    } else {
        read_nvidia_mode(project_dir)
    };
    let (mode, warning) = resolve_nvidia_mode(explicit, remembered);
    if let Some(w) = warning {
        eprintln!("warning: {w}");
    }
    ap_build::redist::set_enabled(mode == NvidiaMode::Redist);
    if mode == NvidiaMode::Redist {
        eprintln!(
            "info: NVIDIA CUDA libraries are packaged from NVIDIA's archives (--nvidia redist)"
        );
    }
    Ok(mode)
}

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

/// Exit status of a run stopped because the agent cannot work (claude is not
/// logged in): distinct from 130 (interrupted) and 1 (an ordinary failure).
const EXIT_AGENT_UNAVAILABLE: i32 = 2;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let mut summary: Option<String> = None;
    let result = run(cli, &mut summary);
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
    let statuses = ap_build::osc::get_project_build_statuses(
        project,
        distro,
        arch,
        ap_build::proc::Emit::quiet(),
    )
    .ok();
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
                // Empty the directory rather than removing it. The lock file and
                // the marker are kept: the marker is what lets a later --cleanup
                // run proceed if one of these removes fails part-way. It is
                // rewritten below once everything else has gone.
                for entry in std::fs::read_dir(&project_dir)? {
                    let path = entry?.path();
                    if path
                        .file_name()
                        .is_some_and(|n| n == PROJECT_LOCK_NAME || n == OWNERSHIP_MARKER)
                    {
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
                                                outcome: PackageOutcome::Failed { reason },
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
        Commands::Update(args) => update_project::run(args, summary)?,
    }

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
        ..
    } = a;
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

/// Branch the distro package `request` names from the linked project that ships
/// it into `project`, and the config to build the branch as: same version, so
/// the existing spec is carried forward, with `patch_request` set so the
/// pre-build turn patches it (convention 62). The error is why that is not
/// possible.
fn branch_for_patch(
    dep: &str,
    request: &ap_build::agent::PatchRequest,
    project: &str,
    config: &PipelineConfig,
    own_listing: &std::collections::HashSet<String>,
    linked_listings: &[(String, std::sync::Arc<std::collections::HashSet<String>>)],
    emit: ap_build::proc::Emit<'_>,
) -> Result<PipelineConfig, String> {
    let rpm = ap_build::osc::rpm_name(dep);
    // A package this project already builds, with a defect a dependent found
    // (an RPM that lacks files it needs, a spec left unfinished): it is fixed
    // where it is, from its own recipe, not branched from anywhere.
    let own_key = ap_build::osc::fuzzy_key(&rpm);
    if let Some(real) = own_listing
        .iter()
        .find(|p| {
            let k = ap_build::osc::fuzzy_key(p);
            k == own_key || k == ap_build::osc::fuzzy_key(dep)
        })
        .cloned()
    {
        let version = ap_build::update::spec_for_package(project, &real, emit)
            .and_then(|text| ap_build::spec::parse_header(&text))
            .map(|h| h.version)
            .ok_or_else(|| format!("cannot read the Version of {project}/{real}"))?;
        ap_core::diag!("  fixing {real} {version} in {project} for another package");
        return Ok(PipelineConfig {
            pkg_name: dep.to_string(),
            rpm_name: real.to_lowercase(),
            version,
            source_repo: None,
            subprojects: Vec::new(),
            force: true,
            regen: false,
            patch_request: Some(request.problem.clone()),
            patch_condition: request.condition.clone(),
            ..config.clone()
        });
    }
    let (origin, shipped) = linked_listings
        .iter()
        .find_map(|(path_project, listing)| {
            listing
                .iter()
                .find(|p| {
                    let k = ap_build::osc::fuzzy_key(p);
                    k == ap_build::osc::fuzzy_key(&rpm) || k == ap_build::osc::fuzzy_key(dep)
                })
                .map(|p| (path_project.clone(), p.clone()))
        })
        .ok_or_else(|| {
            format!(
                "no linked project of {project} ships it, so there is nothing to branch and                  patch (the package that asked for the patch: {})",
                request.problem
            )
        })?;
    // The branch lives under the spelling the project already uses, if any.
    let real = ap_build::osc::obs_name(own_listing, &shipped);
    match ap_build::osc::branch_package(&origin, &shipped, project, false, emit) {
        Ok(ap_build::osc::BranchResult::Branched(b)) if b.target_package != shipped => {
            return Err(format!(
                "OBS branched {origin}/{shipped} as {}, not {shipped}; delete it and patch                  {shipped} by hand",
                b.target_package
            ));
        }
        Ok(_) => {}
        Err(e) => {
            return Err(format!(
                "cannot branch {origin}/{shipped} into {project}: {e}"
            ));
        }
    }
    ap_build::osc::invalidate_project_packages_cache(project);
    let version = ap_build::update::spec_for_package(project, &real, emit)
        .and_then(|text| ap_build::spec::parse_header(&text))
        .map(|h| h.version)
        .ok_or_else(|| format!("cannot read the Version of {project}/{real} after branching it"))?;
    ap_core::diag!("  patching {real} {version} (branched from {origin}) for another package");
    Ok(PipelineConfig {
        pkg_name: dep.to_string(),
        rpm_name: real.to_lowercase(),
        version,
        source_repo: None,
        subprojects: Vec::new(),
        // The branch builds green unchanged; the patch must still be made.
        force: true,
        regen: false,
        patch_request: Some(request.problem.clone()),
        patch_condition: request.condition.clone(),
        ..config.clone()
    })
}

/// Dependencies known to be native (C/C++) libraries, by `fuzzy_key`, as the
/// pipeline reported them: a translated name no longer shows it, and the queue
/// would look it up on PyPI first.
#[derive(Default)]
struct NativeNames {
    keys: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl NativeNames {
    fn record(&self, names: &[String]) {
        let mut keys = self.keys.borrow_mut();
        for n in names {
            keys.insert(ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n)));
        }
    }

    fn contains(&self, dep: &str) -> bool {
        self.keys
            .borrow()
            .contains(&ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(
                dep,
            )))
    }
}

/// Dependencies the repository has at a version older than a package's
/// constraint wants, by `fuzzy_key`: they are updated, not built from scratch
/// or dropped as available.
#[derive(Default)]
struct OutdatedNames {
    /// By `fuzzy_key`, with the release the constraint names, when it names one.
    keys: std::cell::RefCell<std::collections::HashMap<String, Option<String>>>,
}

impl OutdatedNames {
    fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    fn record(&self, names: &[(String, Option<String>)]) {
        let mut keys = self.keys.borrow_mut();
        for (n, required) in names {
            keys.insert(Self::key(n), required.clone());
        }
    }

    fn contains(&self, dep: &str) -> bool {
        self.keys.borrow().contains_key(&Self::key(dep))
    }

    fn required(&self, dep: &str) -> Option<String> {
        self.keys.borrow().get(&Self::key(dep)).cloned().flatten()
    }
}

/// The config to bring `dep`, which the repository has at an older version than
/// a dependent needs, up to the newest PyPI release: its existing recipe is
/// carried forward (a package of this project as it is, a distro package after
/// the pipeline branches it). The error says why that is not possible.
fn update_config_for_outdated(
    dep: &str,
    required: Option<&str>,
    project: &str,
    config: &PipelineConfig,
    own_listing: &std::collections::HashSet<String>,
    reporter: &dyn Reporter,
    emit: ap_build::proc::Emit<'_>,
) -> Result<PipelineConfig, String> {
    let rpm = ap_build::osc::rpm_name(dep);
    let real = own_listing
        .iter()
        .find(|p| {
            let k = ap_build::osc::fuzzy_key(p);
            k == ap_build::osc::fuzzy_key(&rpm) || k == ap_build::osc::fuzzy_key(dep)
        })
        .cloned()
        .unwrap_or(rpm);
    // The release the constraint names, when PyPI has it: the packages of one
    // release family are pinned to each other, and the newest of one may not
    // match the rest. Otherwise the newest release.
    let named = required.filter(|v| {
        ap_build::pipeline::fetch_pypi_json(dep, v)
            .ok()
            .flatten()
            .is_some()
    });
    let newest = match named {
        Some(v) => v.to_string(),
        None => pypi_latest_version(dep, reporter)
            .ok_or_else(|| "it has no PyPI release to update to".to_string())?,
    };
    let current = ap_build::update::spec_for_package(project, &real, emit)
        .and_then(|text| ap_build::spec::parse_header(&text))
        .map(|h| h.version);
    if current.as_deref() == Some(newest.as_str()) {
        return Err(format!(
            "{real} is already at the newest release ({newest}); a newer one than PyPI has \
             cannot be built"
        ));
    }
    ap_core::diag!(
        "  updating {real} from {} to {newest} for a dependent that needs a newer one",
        current.as_deref().unwrap_or("the distro's version")
    );
    Ok(PipelineConfig {
        pkg_name: dep.to_string(),
        rpm_name: real.to_lowercase(),
        version: newest,
        source_repo: None,
        subprojects: Vec::new(),
        force: true,
        regen: false,
        patch_request: None,
        patch_condition: None,
        ..config.clone()
    })
}

/// The dependencies a package asked to have patched instead of built
/// (convention 62), by the name's `fuzzy_key` without its `python-` prefix, so
/// the dependency queue can tell them from the ones it builds.
#[derive(Default)]
struct PatchRequests {
    by_key: std::cell::RefCell<std::collections::HashMap<String, ap_build::agent::PatchRequest>>,
}

impl PatchRequests {
    fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    fn record(&self, patches: &[ap_build::agent::PatchRequest]) {
        let mut by_key = self.by_key.borrow_mut();
        for p in patches {
            by_key.insert(Self::key(&p.name), p.clone());
        }
    }

    fn get(&self, dep: &str) -> Option<ap_build::agent::PatchRequest> {
        self.by_key.borrow().get(&Self::key(dep)).cloned()
    }
}

/// Why each dependency left the dependency queue without being built (or what
/// became of it when it was), kept so that the message that finally gives up on
/// a package can say why every one of its dependencies is out of reach instead
/// of pointing at earlier output that never said.
///
/// Keyed by the name with its separators and `python-` prefix ignored, since
/// the same project comes back spelled differently from one agent turn to the
/// next. The first reason recorded for a dependency is kept: it is the reason
/// it was dropped; later drops of the same name only repeat that it was seen
/// before. [`DepDrops::note_outcome`] overwrites, as the outcome of a build.
#[derive(Default)]
struct DepDrops {
    reasons: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl DepDrops {
    fn key(dep: &str) -> String {
        ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep))
    }

    /// Record why `dep` is dropped from the queue and report it as a warning.
    fn dropped(&self, reporter: &dyn Reporter, dep: &str, reason: impl Into<String>) {
        let reason = reason.into();
        reporter.report(&Event::Warning {
            step: None,
            message: format!("dependency {dep} dropped from the build queue: {reason}"),
        });
        self.reasons
            .borrow_mut()
            .entry(Self::key(dep))
            .or_insert(reason);
    }

    /// Record what became of a dependency that was queued and built.
    fn note_outcome(&self, dep: &str, outcome: impl Into<String>) {
        self.reasons
            .borrow_mut()
            .insert(Self::key(dep), outcome.into());
    }

    /// The reason recorded for `dep`, if any.
    fn reason(&self, dep: &str) -> Option<String> {
        self.reasons.borrow().get(&Self::key(dep)).cloned()
    }

    /// `deps` with the reason each is out of reach, one per line, for a
    /// message that gives up on the package needing them.
    fn explain(&self, deps: &[String]) -> String {
        deps.iter()
            .map(|d| {
                let why = self.reason(d).unwrap_or_else(|| {
                    "queued earlier this run, but nothing was recorded about what \
                     became of it"
                        .to_string()
                });
                format!("  - {d}: {why}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
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
        PipelineResult::Failed { reason, .. } => PackageOutcome::Failed {
            reason: reason.clone(),
        },
    }
}

/// Lists what this run built, what is queued and what the project already has
/// built for this distro and arch, so the outcome is readable without
/// scrolling the log.
/// The sections of the last rpmlint run in a `rpmlint.log`: every section
/// carrying the `unix time` of the last header, so each flavor of a multibuild
/// package counts, not only the one written last.
fn latest_rpmlint_run(text: &str) -> String {
    const HEADER: &str = "=== rpmlint of";
    let stamp = |section: &str| -> Option<String> {
        let header = section.lines().next()?;
        let at = header.rfind("unix time ")? + "unix time ".len();
        Some(header[at..].trim_end_matches([')', ' ', '=']).to_string())
    };
    let mut sections: Vec<&str> = Vec::new();
    let mut start = None;
    for (at, _) in text.match_indices(HEADER) {
        if (at == 0 || text.as_bytes()[at - 1] == b'\n')
            && let Some(s) = start.replace(at)
        {
            sections.push(&text[s..at]);
        }
    }
    if let Some(s) = start {
        sections.push(&text[s..]);
    }
    let Some(last) = sections.last().and_then(|s| stamp(s)) else {
        return String::new();
    };
    sections
        .iter()
        .filter(|s| stamp(s).as_deref() == Some(last.as_str()))
        .copied()
        .collect()
}

/// The error lines of the latest rpmlint run (all its flavors) in `state/<pkg>/rpmlint.log`
/// of each built package, as `pkg: line`.
fn rpmlint_errors_of(state_root: &Path, built: &[&String]) -> Vec<String> {
    let mut out = Vec::new();
    for pkg in built {
        let Some(ws) = ap_build::workspace::Workspace::find_existing(state_root, &[pkg.as_str()])
        else {
            continue;
        };
        let text = std::fs::read_to_string(ws.dir().join("rpmlint.log")).unwrap_or_default();
        let last = latest_rpmlint_run(&text);
        out.extend(
            ap_build::osc::rpmlint_errors(&last)
                .into_iter()
                .map(|l| format!("{pkg}: {l}")),
        );
    }
    out
}

/// A package's dependencies as the summary lists them: one that failed this run
/// reads `failed X`, so a package waiting on it is not mistaken for one that
/// will get its turn.
fn waiting_list(deps: &[String], failed: &[&String]) -> String {
    let key = |n: &str| ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n));
    deps.iter()
        .map(|d| {
            if failed.iter().any(|b| key(b) == key(d)) {
                format!("failed {d}")
            } else {
                d.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_run_summary(
    memory: &MemorySink,
    project: &str,
    distro: &str,
    arch: &str,
    state_root: &Path,
) -> String {
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
    let failed: Vec<&String> = latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::Failed { .. } => Some(pkg),
            _ => None,
        })
        .collect();
    let _ = writeln!(out, "\nQueued, waiting on dependencies ({}):", queued.len());
    for (pkg, deps) in queued {
        if deps.is_empty() {
            let _ = writeln!(out, "  {pkg}");
        } else {
            let _ = writeln!(out, "  {pkg} <- {}", waiting_list(&deps, &failed));
        }
    }

    let lint = rpmlint_errors_of(state_root, &built);
    if !lint.is_empty() {
        let _ = writeln!(out, "\nrpmlint errors in the final RPMs ({}):", lint.len());
        for line in &lint {
            let _ = writeln!(out, "  {line}");
        }
    }

    // OBS reports names in lower case, and this run's packages are already in
    // the built list above, so they are not repeated here.
    let statuses = ap_build::osc::get_project_build_statuses(
        project,
        distro,
        arch,
        ap_build::proc::Emit::quiet(),
    )
    // Only the "already built" extras come from this listing; without it the
    // summary still lists everything this run built.
    .unwrap_or_default();
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

/// The name a package is run, tracked and keyed under: its lowercase RPM guess
/// (see [`ap_build::osc::rpm_name`]). The spelling OBS lists a package under is
/// not used here: the pipeline resolves it from the own project's listing for
/// the OBS calls, and keeping one key per package keeps its workspace, state,
/// run directory and rows in one place (`python-APScheduler` and
/// `python-apscheduler` are the same package).
fn run_name(dep: &str) -> String {
    ap_build::osc::rpm_name(dep)
}

/// The selected targets that are neither running nor already finished in this
/// round: the ones the scheduler may start now, each with the name it is run
/// under (see [`run_name`]), which is how a running or finished package is
/// tracked.
fn unstarted_targets(
    targets: Vec<ap_build::pipeline::TargetInfo>,
    in_flight: &[String],
    done_this_round: &std::collections::HashSet<String>,
) -> Vec<(ap_build::pipeline::TargetInfo, String)> {
    targets
        .into_iter()
        .map(|t| {
            let rpm = run_name(&t.name);
            (t, rpm)
        })
        .filter(|(_, rpm)| !in_flight.contains(rpm) && !done_this_round.contains(rpm))
        .collect()
}

/// Forwards every event to `inner` and remembers whether a package was
/// selected to start, so a retry that returns `Ok` without starting anything
/// can be told apart from one that built something.
struct StartWatch {
    inner: Arc<dyn Reporter + Send + Sync>,
    started: std::sync::atomic::AtomicBool,
}

impl StartWatch {
    fn new(inner: Arc<dyn Reporter + Send + Sync>) -> Self {
        Self {
            inner,
            started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn started(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Reporter for StartWatch {
    fn report(&self, event: &Event) {
        if matches!(event, Event::TargetSelected { .. }) {
            self.started.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.report(event);
    }
}

/// Wait for every package still running, so none is left running unobserved
/// when the build stops on an error. Their results are not used.
fn drain_in_flight(
    rx: &std::sync::mpsc::Receiver<(String, String, PipelineResult)>,
    in_flight: &mut Vec<String>,
) {
    while !in_flight.is_empty() {
        match rx.recv() {
            Ok((rpm_name, _, _)) => in_flight.retain(|r| *r != rpm_name),
            Err(_) => break,
        }
    }
}

/// A failed add request (the `n` key or a retry), kept until a later build of
/// its package settles it.
struct AddFailure {
    /// The name the request was made under (see `AddRequest`).
    label: String,
    message: String,
    /// How many `PackageFinished` events the run had reported when it failed.
    after: usize,
}

/// Whether `row` reported `PackageFinished` among `finished[from..]`. `finished`
/// is the run's record of `PackageFinished` events, in order.
fn finished_since(finished: &[Event], from: usize, row: &str) -> bool {
    finished
        .iter()
        .skip(from)
        .any(|e| matches!(e, Event::PackageFinished { pkg, .. } if pkg == row))
}

/// Drops the failures that a later build settled. A failure goes when a row it
/// was requested as (the request name, or the row's own RPM name) has its last
/// `PackageFinished` at or after the failure, and that outcome is built. `ends`
/// is the run's `PackageFinished` events in order, as (row, built).
fn withdraw_settled_failures(
    failures: &mut Vec<AddFailure>,
    ends: &[(String, bool)],
    label_of: impl Fn(&str) -> Option<String>,
) {
    let mut last: std::collections::HashMap<&str, (usize, bool)> = Default::default();
    for (at, (row, built)) in ends.iter().enumerate() {
        last.insert(row.as_str(), (at, *built));
    }
    failures.retain(|f| {
        !last.iter().any(|(row, (at, built))| {
            // A row has a recorded name when it was requested or built as a
            // dependency under one; otherwise the label names the package through
            // its RPM name (`foo` is `python-foo`), compared without case or
            // separators.
            let named = match label_of(row) {
                Some(label) => label == f.label,
                None => {
                    ap_build::osc::fuzzy_key(&ap_build::osc::rpm_name(&f.label))
                        == ap_build::osc::fuzzy_key(row)
                }
            };
            *built && *at >= f.after && (*row == f.label.as_str() || named)
        })
    });
}

/// The `PackageFinished` events in order, as (row, ended built).
fn row_ends(finished: &[Event]) -> Vec<(String, bool)> {
    finished
        .iter()
        .filter_map(|e| match e {
            Event::PackageFinished { pkg, outcome, .. } => Some((
                pkg.clone(),
                matches!(
                    outcome,
                    PackageOutcome::Built | PackageOutcome::AlreadyBuilt
                ),
            )),
            _ => None,
        })
        .collect()
}

/// [`withdraw_settled_failures`] over the events the run has reported so far.
fn withdraw_settled(
    failures: &mut Vec<AddFailure>,
    memory: &MemorySink,
    tui: &Option<tui::TuiController>,
) {
    withdraw_settled_failures(failures, &row_ends(&memory.finished()), |row| {
        tui.as_ref()
            .and_then(|t| t.label_of(row).or_else(|| t.origin_of(row)))
    });
}

/// The message of a retry that has no request name to resend. A dependency
/// row was never requested by name, and building its RPM name as a request
/// builds a different package (`python-foo` is `foo` on PyPI).
const NO_REQUEST_NAME: &str = "dependency rows cannot be retried from the TUI: rerun the build";

/// The name a TUI request is built under, and the row it retries (if it is a
/// retry). A retry takes the name its row was requested with, from `origin_of`;
/// a row with none gets no name, so nothing is built for it.
fn request_names(
    req: tui::AddRequest,
    origin_of: impl Fn(&str) -> Option<String>,
) -> (Option<String>, Option<String>) {
    match req {
        tui::AddRequest::New(name) => (Some(name), None),
        tui::AddRequest::Retry(row) => (origin_of(&row), Some(row)),
    }
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
    // The name this build was requested under, before discovery below may
    // turn it into a repository. A TUI retry resends it (see `tui_ctrl`).
    let request_name = source_repo.clone().unwrap_or_else(|| name.to_string());
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
            } else if let Some(v) =
                ap_build::redist::latest_version(&ap_build::redist::Network, name)
            {
                // `--nvidia-redist`: NVIDIA's version of the component.
                v.map_err(|e| color_eyre::eyre::eyre!(e))?
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
                let pypi_version = pypi_latest_version(name, reporter.as_ref());
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
    // The project's listing is read here so an unreadable one stops the run
    // before anything is named. The name itself is not taken from it: the
    // pipeline resolves the spelling OBS lists the package under (a package
    // OBS has as `python-APScheduler` is checked out under that spelling).
    ap_build::osc::try_list_project_packages(project, ap_build::proc::Emit::run(reporter.as_ref()))
        .map_err(|e| color_eyre::eyre::eyre!(e))?;
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
        agent_backend: AgentBackend::Cli,
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
        )?
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

    // Resolve closure, select all buildable targets, build them in parallel,
    // then repeat until the entire closure is built (or an error occurs).
    loop {
        // Before re-resolving, build any runtime-discovered deps (e.g. test-only
        // deps the agent found to be unresolvable).
        // Groups of dependencies set aside until what they wait on has had its
        // batch (see `retry_group` below). The most recent group runs first
        // once nothing else is pending: it waits on the newest packages, and a
        // package queued behind it may wait on it in turn.
        let mut retry_stack: Vec<Vec<String>> = Vec::new();
        while !pending_deps.is_empty() || !retry_stack.is_empty() {
            if pending_deps.is_empty() {
                pending_deps = retry_stack.pop().unwrap_or_default();
            }
            let batch = std::mem::take(&mut pending_deps);
            let mut newly_unbuildable: Vec<String> = Vec::new();
            // (dependency, the packages it waits on) for each dependency OBS
            // reports unresolvable in this repository/architecture.
            let mut deferred: Vec<(String, Vec<String>)> = Vec::new();
            // Everything the availability filter below needs is read once for
            // the batch, up front. A listing that cannot be read stops the
            // run: treated as empty, it would queue packages OBS already has.
            let lookup_emit = ap_build::proc::Emit::run(reporter.as_ref());
            let own_listing = ap_build::osc::try_list_project_packages(project, lookup_emit)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            let own_statuses =
                ap_build::osc::get_project_build_statuses(project, distro, arch, lookup_emit)?;
            let mut linked_listings = Vec::new();
            for (path_project, _repo) in
                ap_build::osc::get_project_repo_paths(project, lookup_emit)?
            {
                let listing = ap_build::osc::try_list_project_packages(&path_project, lookup_emit)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                linked_listings.push((path_project, listing));
            }
            // What a dependency build runs as: its own thread, reported as soon
            // as it is done. Reporting in join order let a slower sibling hold
            // back every package finished after it, so their rows stayed on
            // their last step.
            let spawn_dep_build = |dep_config: PipelineConfig, dep: String| {
                let dep_rpm = dep_config.rpm_name.clone();
                let dep_pkg_run = run_dir.pkg(&dep_rpm);
                if let Err(e) = dep_pkg_run.ensure() {
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!(
                            "its run directory {} could not be created: {e}",
                            dep_pkg_run.path.display()
                        ),
                    );
                    return None;
                }
                reporter.report(&Event::TargetSelected {
                    pkg: dep_rpm.clone(),
                    version: dep_config.version.clone(),
                    reason: SelectionReason::FirstUnbuiltDependency,
                    blocked_by: vec![],
                });
                // A failed request under the dependency's name is settled by this
                // row (see `withdraw_settled_failures`). It is not a request, so
                // a retry of this row must not resend the name (see `request_names`).
                if let Some(t) = &tui_ctrl {
                    t.note_label(&dep_rpm, &dep);
                }
                let dep_version = dep_config.version.clone();
                let r = pkg_reporter(&dep_rpm);
                let sem2 = sem.clone();
                let finished_name = dep_rpm.clone();
                let finished_version = dep_version.clone();
                let handle = std::thread::spawn(move || {
                    let _permit = Permit::acquire(&sem2);
                    // A panic is a failure of this row, reported like any other
                    // outcome, so the row settles and the run sees it finish.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_package(&dep_config, &dep_pkg_run, r.as_ref())
                    }))
                    .unwrap_or_else(|p| PipelineResult::Failed {
                        rpm_name: finished_name.clone(),
                        reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
                        kind: FailureKind::Other,
                    });
                    r.report(&Event::PackageFinished {
                        pkg: finished_name,
                        version: finished_version,
                        outcome: package_outcome(&result),
                    });
                    result
                });
                Some((dep_rpm, dep_version, handle, dep))
            };
            // A dependency a package asked to have patched is branched from the
            // linked project that ships it, not looked up on PyPI or a forge.
            let (patch_batch, batch): (Vec<String>, Vec<String>) = batch
                .into_iter()
                .partition(|d| patch_requests.get(d).is_some());
            let mut patch_handles = Vec::new();
            for rpm_dep in patch_batch {
                let dep = ap_build::osc::strip_rpm_prefix(&rpm_dep).to_string();
                let Some(request) = patch_requests.get(&dep) else {
                    continue;
                };
                if !attempted_deps.insert(ap_build::osc::fuzzy_key(&dep)) {
                    let earlier = dep_drops
                        .reason(&dep)
                        .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!("it was already attempted this run ({earlier})"),
                    );
                    continue;
                }
                match branch_for_patch(
                    &dep,
                    &request,
                    project,
                    &config,
                    &own_listing,
                    &linked_listings,
                    lookup_emit,
                ) {
                    Ok(dep_config) => {
                        if let Some(h) = spawn_dep_build(dep_config, dep.clone()) {
                            patch_handles.push(h);
                        }
                    }
                    Err(why) => {
                        dep_drops.dropped(reporter.as_ref(), &dep, why);
                    }
                }
            }
            // A dependency the repository has at an older version than a
            // dependent needs is updated, not dropped as available.
            let (outdated_batch, batch): (Vec<String>, Vec<String>) =
                batch.into_iter().partition(|d| outdated_names.contains(d));
            for rpm_dep in outdated_batch {
                let dep = ap_build::osc::strip_rpm_prefix(&rpm_dep).to_string();
                if !attempted_deps.insert(ap_build::osc::fuzzy_key(&dep)) {
                    let earlier = dep_drops
                        .reason(&dep)
                        .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                    dep_drops.dropped(
                        reporter.as_ref(),
                        &dep,
                        format!("it was already attempted this run ({earlier})"),
                    );
                    continue;
                }
                match update_config_for_outdated(
                    &dep,
                    outdated_names.required(&dep).as_deref(),
                    project,
                    &config,
                    &own_listing,
                    reporter.as_ref(),
                    lookup_emit,
                ) {
                    Ok(dep_config) => {
                        if let Some(h) = spawn_dep_build(dep_config, dep.clone()) {
                            patch_handles.push(h);
                        }
                    }
                    Err(why) => dep_drops.dropped(reporter.as_ref(), &dep, why),
                }
            }
            // Every reported name is triaged once (see `depclass::triage`):
            // the `python<ver>-` prefix and `-devel` wrapping come off, flavors
            // and repeats of one package collapse, and what is not something to
            // build (a capability, an interpreter package, a vendor library, a
            // system tool, something the distro provides) is dropped with its
            // reason. Each of those is final for the run, so it counts as
            // attempted: a package that keeps re-reporting the name is then
            // recognized as making no progress (see `new_deps` below).
            let mut in_batch = std::collections::HashSet::new();
            let mut triaged: Vec<String> = Vec::new();
            // Names `triage` found to be native libraries, by `fuzzy_key`.
            let mut native_keys: std::collections::HashSet<String> = Default::default();
            for raw in batch {
                match ap_build::depclass::triage(&raw, &|c| {
                    ap_build::osc::available_to_project(project, arch, c, lookup_emit)
                }) {
                    ap_build::depclass::Triage::Build { name, kind } => {
                        if kind == ap_build::depclass::PackageKind::Native
                            || native_names.contains(&name)
                        {
                            native_keys.insert(ap_build::osc::fuzzy_key(&name));
                        }
                        if in_batch.insert(ap_build::osc::fuzzy_key(&name)) {
                            triaged.push(name);
                        }
                    }
                    ap_build::depclass::Triage::Skip(ap_build::depclass::Skip::Empty) => {}
                    ap_build::depclass::Triage::Skip(skip) => {
                        attempted_deps.insert(ap_build::osc::fuzzy_key(&raw));
                        dep_drops.dropped(reporter.as_ref(), &raw, skip.reason());
                        if skip.is_unbuildable() && !newly_unbuildable.contains(&raw) {
                            newly_unbuildable.push(raw);
                        }
                    }
                }
            }
            // A native library the projects do not publish under this name but
            // do publish under similar ones (`ffmpeg` against `ffmpeg-8`,
            // `ffmpeg-8-mini-devel`) is the distro's own package: building it
            // from source here would be a second, worse copy.
            triaged.retain(|dep| {
                if !native_keys.contains(&ap_build::osc::fuzzy_key(dep)) {
                    return true;
                }
                if ap_build::osc::project_ships_binary(project, arch, dep, lookup_emit)
                    != Some(false)
                {
                    return true;
                }
                let similar =
                    ap_build::osc::similar_published_binaries(project, arch, dep, lookup_emit);
                if similar.is_empty() {
                    return true;
                }
                attempted_deps.insert(ap_build::osc::fuzzy_key(dep));
                dep_drops.dropped(
                    reporter.as_ref(),
                    dep,
                    format!(
                        "the distro ships it under other names ({}), which a package \
                         requires instead of building a copy from source",
                        similar.join(", ")
                    ),
                );
                false
            });
            // Dependencies dropped only because a build of them is already
            // underway: a later request for one must not be refused as "already
            // attempted" if that build then fails.
            let mid_build: std::cell::RefCell<Vec<String>> = Default::default();
            let handles: Vec<_> = triaged
                .into_iter()
                .filter(|dep| {
                    // A dependency we already built here is available, even while
                    // OBS has not indexed it yet. Its version is logged so a bound
                    // it does not meet is still visible.
                    match built_locally(state_root, &ap_build::osc::rpm_name(dep)) {
                        Some(version) => {
                            dep_drops.dropped(
                                reporter.as_ref(),
                                dep,
                                format!("it was already built locally at {version}"),
                            );
                            false
                        }
                        None => true,
                    }
                })
                .filter(|dep| {
                    // Counted as attempted whatever a later filter does with
                    // it, so a package that keeps re-reporting a name that is
                    // already satisfied (in the project, a linked project)
                    // is still recognized as making no progress instead of
                    // retrying forever.
                    let fresh = attempted_deps.insert(ap_build::osc::fuzzy_key(dep));
                    if !fresh {
                        let earlier = dep_drops
                            .reason(dep)
                            .unwrap_or_else(|| "no outcome was recorded for it".to_string());
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!("it was already attempted this run ({earlier})"),
                        );
                    }
                    fresh
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
                    // Own-project packages are matched by fuzzy key so a
                    // separator mismatch can't false-miss; of several
                    // spellings the built one counts (see `availability`).
                    let found = ap_build::depclass::availability(
                        dep,
                        &own_listing,
                        &own_statuses,
                        &linked_listings,
                    );
                    if let ap_build::depclass::Availability::InProject {
                        pkg: real_pkg,
                        status,
                    } = &found
                    {
                        let real_pkg = real_pkg.as_str();
                        let status = status.as_ref();
                        // Package exists in our project. Only skip queuing
                        // it when its OBS build status is actually
                        // resolvable — i.e. there's a built binary (or one
                        // genuinely in flight). An Unresolvable/Failed/
                        // Broken/Disabled/Excluded/Unknown or missing-
                        // status entry stays queued so the agent can look
                        // at it.
                        // The status is the one for the repository and
                        // architecture being built (`own_statuses` was read
                        // for exactly that pair): the same package being
                        // excluded or unresolvable on another one says
                        // nothing about this build.
                        let skip = ap_build::pipeline::status_is_resolvable(status);
                        if matches!(status, Some(ap_build::osc::BuildStatus::Excluded)) {
                            // No rebuild changes a project/spec exclusion, and
                            // whatever needs this package cannot be built
                            // here either; say so instead of running a build
                            // that can only report it.
                            dep_drops.dropped(
                                reporter.as_ref(),
                                dep,
                                format!(
                                    "{real_pkg} is excluded for {distro}/{arch} in {project} \
                                     (ExclusiveArch/ExcludeArch in its spec, or the project \
                                     config), so nothing that needs it can be built for \
                                     {arch}"
                                ),
                            );
                            return false;
                        }
                        if matches!(status, Some(ap_build::osc::BuildStatus::Unresolvable))
                            && !deferred_once.contains(&bare_key)
                        {
                            // The package exists but cannot build because
                            // packages it needs are missing; OBS names them.
                            // Build those first, then come back to it.
                            let blockers: Vec<String> = ap_build::osc::unresolvable_blockers(
                                project, real_pkg, distro, arch,
                            )
                            .into_iter()
                            .filter(|b| {
                                ap_build::osc::built_in_project(&own_listing, &own_statuses, b)
                                    .is_none()
                            })
                            .collect();
                            if !blockers.is_empty() {
                                deferred_once.insert(bare_key.clone());
                                reporter.report(&Event::Progress {
                                    step: None,
                                    message: format!(
                                        "{dep} exists in {project} as {real_pkg} but is \
                                         unresolvable for {distro}/{arch}: building what it \
                                         waits on first ({})",
                                        blockers.join(", ")
                                    ),
                                });
                                deferred.push((dep.clone(), blockers));
                                return false;
                            }
                        }
                        if skip {
                            let mut reason = format!(
                                "it is already in {project} as {real_pkg} with status \
                                 {:?}, which counts as resolvable",
                                status.map(|s| s.to_string()).unwrap_or_default()
                            );
                            // Built is not the same as usable: a skeleton spec
                            // "succeeds" with an empty RPM.
                            if matches!(status, Some(ap_build::osc::BuildStatus::Succeeded))
                                && let Some(unusable) = ap_build::osc::empty_package_warning(
                                    project, real_pkg, distro, arch,
                                )
                            {
                                reason = format!("{reason}, but {unusable}");
                            }
                            dep_drops.dropped(reporter.as_ref(), dep, reason);
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
                    // comment above). That only holds for a requirement
                    // without a version bound: when the package that needs it
                    // still reports it unresolvable afterwards, the shipped
                    // version is too old, which the give-up message says.
                    if let ap_build::depclass::Availability::InLinked {
                        project: path_project,
                        pkg: match_name,
                    } = &found
                    {
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!(
                                "it is available from the linked project {path_project} as \
                                 {match_name} (if the package needing it still reports it \
                                 unresolvable, that version does not meet its bound: it needs \
                                 a newer release built in {project}, or a patch_dependencies \
                                 request)"
                            ),
                        );
                        return false;
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
                    let keep = !state_path.exists() || retrying.contains(&bare_key);
                    if !keep {
                        mid_build.borrow_mut().push(bare_key.clone());
                        dep_drops.dropped(
                            reporter.as_ref(),
                            dep,
                            format!(
                                "a local workspace at {} already exists, so it is \
                                 probably mid-build through the ordinary frontier, which \
                                 keeps retrying it on its own",
                                state_path.display()
                            ),
                        );
                    }
                    keep
                })
                .filter(|dep| {
                    // `update`: a dependency another project already
                    // publishes becomes a build path of the branch project, or
                    // is already reachable, instead of a second copy built here.
                    let Some(resolver) = branch.and_then(|b| b.resolver.as_ref()) else {
                        return true;
                    };
                    let mut candidates = vec![dep.clone(), ap_build::osc::rpm_name(dep)];
                    candidates.extend(cover_flavors.iter().map(|f| format!("{f}-{dep}")));
                    match resolver.cover_dependency(&candidates) {
                        Ok(Some(note)) => {
                            dep_drops.dropped(reporter.as_ref(), dep, note);
                            false
                        }
                        Ok(None) => true,
                        Err(e) => {
                            reporter.report(&Event::Warning {
                                step: None,
                                message: format!(
                                    "could not search OBS for {dep}: {e}; building it instead"
                                ),
                            });
                            true
                        }
                    }
                })
                .filter_map(|dep| {
                    let native = native_keys.contains(&ap_build::osc::fuzzy_key(&dep));
                    let dep_config =
                        match resolve_dependency_config(&dep, native, &config, reporter.as_ref()) {
                            Some(c) => c,
                            None => {
                                // No PyPI project and no discoverable source repo —
                                // this is never going to become buildable no matter
                                // how many times it's reported. Record it so every
                                // package needing it learns that from its next
                                // prompt (convention 28 Exception 4) instead of
                                // reporting it again forever.
                                dep_drops.dropped(
                                    reporter.as_ref(),
                                    &dep,
                                    "it is unresolvable: no PyPI release and no usable public \
                                     source repository (the warning above lists what was tried)",
                                );
                                newly_unbuildable.push(dep);
                                return None;
                            }
                        };
                    spawn_dep_build(dep_config, dep)
                })
                .collect();
            let handles: Vec<_> = handles.into_iter().chain(patch_handles).collect();

            // A request that was only set aside for a build already underway
            // may be made again if that build fails, a bounded number of times.
            for key in mid_build.into_inner() {
                let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                *rounds += 1;
                if *rounds < max_dep_attempts {
                    attempted_deps.remove(&key);
                }
            }

            for dep in newly_unbuildable {
                if !config.unbuildable_deps.contains(&dep) {
                    config.unbuildable_deps.push(dep);
                }
            }

            // Dependencies to run again once what they wait on has had its
            // batch: those OBS reports unresolvable, and (below) those that
            // came back needing packages of their own.
            let mut retry_group: Vec<String> = Vec::new();
            for (dep, blockers) in deferred {
                attempted_deps.remove(&ap_build::osc::fuzzy_key(&dep));
                retrying.insert(ap_build::osc::fuzzy_key(&dep));
                pending_deps.extend(blockers);
                retry_group.push(dep);
            }

            for (dep_rpm, _version, handle, dep) in handles {
                let result = join_package(handle, &dep_rpm);
                // Aborted run: the results are not outcomes of the packages.
                if ap_build::proc::abort_reason().is_some() {
                    continue;
                }
                let key = ap_build::osc::fuzzy_key(&dep);
                match result {
                    PipelineResult::Built { .. } => {
                        ap_core::diag!("built dep {dep_rpm}");
                        dep_drops.note_outcome(
                            &dep_rpm,
                            "it was built this run, yet the package that needs it still \
                             reports it missing (a name mismatch?)",
                        );
                    }
                    PipelineResult::AlreadyBuilt { .. } => {
                        ap_core::diag!("dep {dep_rpm} already built");
                        dep_drops.note_outcome(
                            &dep_rpm,
                            "it was already built, yet the package that needs it still \
                             reports it missing (a name mismatch?)",
                        );
                    }
                    PipelineResult::MissingDeps {
                        deps: more,
                        patches,
                        native,
                        outdated,
                        ..
                    } => {
                        patch_requests.record(&patches);
                        native_names.record(&native);
                        outdated_names.record(&outdated);
                        dep_drops.note_outcome(
                            &dep_rpm,
                            format!("it could not be built until {} exist", more.join(", ")),
                        );
                        // It needs packages of its own: once those have had
                        // their batch it is run again, instead of staying
                        // "attempted" while the package waiting on it is
                        // abandoned. Bounded like any other retry.
                        let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                        *rounds += 1;
                        if !more.is_empty() && *rounds < max_dep_attempts {
                            attempted_deps.remove(&key);
                            retrying.insert(key);
                            retry_group.push(dep);
                        }
                        pending_deps.extend(more);
                    }
                    PipelineResult::Failed { reason, kind, .. } => {
                        // A dependency OBS or the pipeline failed is run
                        // again through the normal pipeline when the package
                        // needing it names it once more, rather than counted
                        // as attempted and dropped after one go — unless it
                        // used up its allowance, which a second round would
                        // only repeat. Rounds are bounded and an attempt
                        // within a round resumes the package's persisted
                        // attempt count.
                        let rounds = dep_rounds.entry(key.clone()).or_insert(0);
                        *rounds += 1;
                        let retry = dep_failure_retries(kind, *rounds, max_dep_attempts);
                        if retry {
                            attempted_deps.remove(&key);
                            retrying.insert(key);
                            // Run again once the rest of this batch has had its
                            // turn, whether or not a dependent names it again:
                            // the dependent may have given up on seeing it failed.
                            retry_group.push(dep.clone());
                        }
                        dep_drops.note_outcome(
                            &dep_rpm,
                            format!(
                                "its build failed ({reason}){}",
                                if retry {
                                    "; it is tried once more if it is still needed"
                                } else {
                                    ""
                                }
                            ),
                        );
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
            if ap_build::proc::abort_reason().is_some() {
                return Err(color_eyre::eyre::eyre!("run aborted"));
            }
            if !retry_group.is_empty() {
                retry_stack.push(retry_group);
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
            let outcome = package_outcome(&result);
            r.report(&Event::PackageFinished {
                pkg: rpm_name.clone(),
                version: version.clone(),
                outcome,
            });
            match result {
                PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => return Ok(()),
                PipelineResult::MissingDeps {
                    deps,
                    patches,
                    native,
                    outdated,
                    ..
                } => {
                    patch_requests.record(&patches);
                    native_names.record(&native);
                    outdated_names.record(&outdated);
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
                             build. Why each one is out of reach:\n{}\n\
                             (no PyPI release AND no discoverable source repo is \
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
                            dep_drops.explain(&deps),
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

        // Select the frontier and build it as a stream: a package starts as soon
        // as everything it needs has been built, rather than when the whole
        // round has finished. Each finished package is handled below and the
        // frontier is selected again. A package that finished in this round is
        // not started again until the next round, so a retry keeps the
        // round-level pacing.
        //
        // `config.force` is passed down so `--force` (and `update`, which
        // always sets it) can override the "OBS says it's already green"
        // resolvability gate for the root -- see `select_target`'s doc
        // comment. Without this, the gate would skip the package before
        // `run_package`'s own force check could ever fire.
        let mut done_this_round: std::collections::HashSet<String> = Default::default();
        let mut spawned_any = false;
        let mut batch_pending: Vec<String> = Vec::new();
        let mut batch_failures: Vec<(String, String)> = Vec::new();
        loop {
            if ap_build::proc::shutting_down() {
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!("interrupted"));
            }
            // The package names OBS lists are what builds and tracking use, so
            // the listing is read with the frontier: a failure of either ends
            // the build, after the packages already running have finished.
            let selected = select_target(
                name,
                explicit_target,
                pinned_version.as_deref(),
                &config,
                &permanently_failed,
                config.force,
                reporter.as_ref(),
            )
            .map_err(|e| color_eyre::eyre::eyre!(e))
            .and_then(|targets| {
                // The listing is still read, so an unreadable project stops
                // the round here as it always has; the names do not use it.
                ap_build::osc::try_list_project_packages(
                    project,
                    ap_build::proc::Emit::run(reporter.as_ref()),
                )
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
                Ok(targets)
            });
            let targets = match selected {
                Ok(v) => v,
                Err(e) => {
                    // Packages already running are not abandoned: let them
                    // finish (their results land in state as usual) first.
                    drain_in_flight(&rx, &mut in_flight);
                    return Err(e);
                }
            };
            let fresh = unstarted_targets(targets, &in_flight, &done_this_round);
            if fresh.is_empty() && in_flight.is_empty() {
                break;
            }

            for (t, rpm_name) in &fresh {
                if let Some(tc) = &tui_ctrl {
                    tc.note_origin(rpm_name, &t.name);
                }
                reporter.report(&Event::TargetSelected {
                    pkg: rpm_name.clone(),
                    version: t.version.clone(),
                    reason: t.reason.clone(),
                    blocked_by: t.blocked_by.clone(),
                });
            }

            // One thread per package; OBS builds run concurrently.
            let mut unspawned = fresh.into_iter();
            while let Some((target_info, rpm_name)) = unspawned.next() {
                let pkg_config = PipelineConfig {
                    pkg_name: target_info.name.clone(),
                    rpm_name: rpm_name.clone(),
                    version: target_info.version.clone(),
                    // The root package's findings are not a dependency's.
                    patch_request: None,
                    patch_condition: None,
                    ..config.clone()
                };
                let pkg_run = run_dir.pkg(&rpm_name);
                if let Err(e) = pkg_run.ensure() {
                    // Every row this round selected must settle: the failing one
                    // and the fresh ones never spawned, or the TUI keeps them
                    // queued for good. Rows already spawned report themselves.
                    let reason = format!(
                        "cannot create the package run directory {}: {e}",
                        pkg_run.path.display()
                    );
                    reporter.report(&Event::PackageFinished {
                        pkg: rpm_name.clone(),
                        version: target_info.version.clone(),
                        outcome: PackageOutcome::Failed {
                            reason: reason.clone(),
                        },
                    });
                    for (rest, rest_rpm) in unspawned {
                        reporter.report(&Event::PackageFinished {
                            pkg: rest_rpm,
                            version: rest.version.clone(),
                            outcome: PackageOutcome::Failed {
                                reason: format!(
                                    "not started because {rpm_name} has no run directory: {e}"
                                ),
                            },
                        });
                    }
                    // Join the workers already spawned so none is left running
                    // unobserved once we return.
                    drain_in_flight(&rx, &mut in_flight);
                    return Err(color_eyre::eyre::eyre!(reason));
                }
                let r = pkg_reporter(&rpm_name);
                let sem2 = sem.clone();
                let tx = tx.clone();
                let version = target_info.version.clone();
                in_flight.push(rpm_name.clone());
                std::thread::spawn(move || {
                    let _permit = Permit::acquire(&sem2);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_package(&pkg_config, &pkg_run, r.as_ref())
                    }))
                    .unwrap_or_else(|p| PipelineResult::Failed {
                        rpm_name: rpm_name.clone(),
                        reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
                        kind: FailureKind::Other,
                    });
                    // Reported as soon as this package is done, not when the
                    // scheduler gets to its result, so a slower sibling never
                    // holds back the row of one that already finished. The
                    // report is itself caught: a panic in a reporter must not
                    // skip the send below, or the scheduler waits on a result
                    // that never comes (it keeps its own sender alive).
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        r.report(&Event::PackageFinished {
                            pkg: rpm_name.clone(),
                            version: version.clone(),
                            outcome: package_outcome(&result),
                        });
                    }));
                    let _ = tx.send((rpm_name, version, result));
                });
            }

            // Wait for one package to finish, then select again: its result may
            // have unblocked others.
            let (rpm_name, version, result) = match rx.recv() {
                Ok(r) => r,
                Err(_) => {
                    // Every worker is gone without a result: fail the oldest
                    // in-flight package rather than panic the scheduler.
                    let Some(rpm_name) = in_flight.first().cloned() else {
                        return Err(color_eyre::eyre::eyre!(
                            "package workers ended without reporting"
                        ));
                    };
                    let result = PipelineResult::Failed {
                        rpm_name: rpm_name.clone(),
                        reason: "build thread ended without reporting a result".to_string(),
                        kind: FailureKind::Other,
                    };
                    (rpm_name, String::new(), result)
                }
            };
            in_flight.retain(|r| *r != rpm_name);
            // A result counts as work done in this pass, whoever started it.
            spawned_any = true;
            // The run was aborted (not an ordinary failure): do not count or
            // retry this result, just wait for the rest and stop.
            if ap_build::proc::abort_reason().is_some() {
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!("run aborted"));
            }
            done_this_round.insert(rpm_name.clone());

            match result {
                PipelineResult::Built { .. } => {
                    ap_core::diag!("built {rpm_name} {version}");
                    fail_counts.remove(&rpm_name);
                }
                PipelineResult::AlreadyBuilt { .. } => {
                    ap_core::diag!("{rpm_name} {version} already built");
                    fail_counts.remove(&rpm_name);
                }
                PipelineResult::MissingDeps {
                    deps,
                    patches,
                    native,
                    outdated,
                    ..
                } => {
                    patch_requests.record(&patches);
                    native_names.record(&native);
                    outdated_names.record(&outdated);
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
                        give_ups.insert(
                            rpm_name.clone(),
                            format!(
                                "it needs {} and none of them can be queued again:\n{}",
                                deps.join(", "),
                                dep_drops.explain(&deps)
                            ),
                        );
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: format!(
                                "giving up on {rpm_name}: it needs {} and each of those was \
                                 already attempted and dropped; anything depending on it \
                                 stays blocked. Why each one is out of reach:\n{}",
                                deps.join(", "),
                                dep_drops.explain(&deps)
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
                    // A package whose resolution ladder was spent is not run
                    // again: the same spec would fail the same ladder again. It
                    // is a failure of the tool to investigate, never a `blocked`.
                    let ladder = reason.starts_with(ap_build::pipeline::LADDER_FAILED_PREFIX);
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
                        give_ups.insert(
                            rpm_name.clone(),
                            if ladder {
                                format!("every resolution rung failed ({reason})")
                            } else if exhausted {
                                format!("it spent its full budget/attempt allowance ({reason})")
                            } else {
                                format!("it failed {count} times in a row (last: {reason})")
                            },
                        );
                        pkg_reporter(&rpm_name).report(&Event::Warning {
                            step: None,
                            message: if ladder {
                                format!(
                                    "{rpm_name} is unresolved: every rung of the resolution \
                                     ladder failed ({reason}); this is a defect of the tool to \
                                     investigate. Anything depending on it stays waiting, the \
                                     rest of the closure keeps going"
                                )
                            } else if exhausted {
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
            // Missing dependencies are handed to the dependency queue now, not
            // when every package still running has finished: a package waiting
            // on `logfire` should not sit behind an hour-long build of another.
            // The packages still running keep running (their results are read
            // when the stream resumes).
            if !batch_pending.is_empty() {
                break;
            }
        }

        // Nothing was started this round: the closure is either built or
        // blocked, which is the terminal state checked here.
        if !spawned_any {
            if permanently_failed.is_empty() {
                ap_core::diag!("{name} and all dependencies are already built");
                return Ok(());
            }
            for pkg in &permanently_failed {
                give_ups
                    .entry(pkg.clone())
                    .or_insert_with(|| "no reason was recorded".to_string());
            }
            return Err(color_eyre::eyre::eyre!(build_incomplete_message(&give_ups)));
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
                // Packages the batch left running are joined before returning.
                drain_in_flight(&rx, &mut in_flight);
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
                drain_in_flight(&rx, &mut in_flight);
                return Err(color_eyre::eyre::eyre!(
                    "{name} needs {} but every one has already been attempted \
                     without success; cannot build. Why each one is out of reach:\n{}",
                    names,
                    dep_drops.explain(&pending_deps)
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
        };
        let ap_build::update::Decision::Update {
            ecosystem,
            new_version,
            ..
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
mod tests {
    #[test]
    fn the_plan_counts_each_kind_of_package_as_it_is_added() {
        use ap_build::update::{Candidate, Decision, Ecosystem};
        let sink = ap_core::sink::MemorySink::new();
        let mut plan = super::PlanReport::default();
        let eco = || Ecosystem::Pypi {
            pypi_name: "x".into(),
        };
        for decision in [
            Decision::Update {
                ecosystem: eco(),
                old_version: "1".into(),
                new_version: "2".into(),
            },
            Decision::UpToDate {
                ecosystem: eco(),
                version: "1".into(),
            },
        ] {
            plan.add(
                &Candidate {
                    rpm_name: "python-x".into(),
                    decision,
                },
                &sink,
            );
        }
        assert_eq!((plan.n_update, plan.n_uptodate, plan.total()), (1, 1, 2));
    }

    #[test]
    fn a_settled_tui_run_exits_zero_only_without_errors() {
        assert!(super::tui_run_exit(true, None, Ok(()), Vec::new()).is_ok());
    }

    #[test]
    fn a_settled_tui_run_with_a_standing_failure_exits_with_it() {
        // A request that failed before it had a row: no retry can fix it.
        let err = super::tui_run_exit(
            true,
            None,
            Ok(()),
            vec!["bad name: invalid package name".to_string()],
        )
        .unwrap_err();
        assert_eq!(format!("{err}"), "bad name: invalid package name");
    }

    #[test]
    fn a_settled_tui_run_whose_root_failed_exits_with_its_error() {
        let err = super::tui_run_exit(true, None, Err(color_eyre::eyre::eyre!("root")), Vec::new())
            .unwrap_err();
        assert_eq!(format!("{err}"), "root");
    }

    #[test]
    fn a_tui_that_died_mid_run_is_judged_by_its_builds() {
        let dead = || Some("render failed".to_string());
        // Every build succeeded: the run is a success, not an unsettled TUI run.
        assert!(super::tui_run_exit(false, dead(), Ok(()), Vec::new()).is_ok());
        // The root or an added package failed: that is the exit.
        let err =
            super::tui_run_exit(false, dead(), Ok(()), vec!["a: first".to_string()]).unwrap_err();
        assert_eq!(format!("{err}"), "a: first");
        let err = super::tui_run_exit(
            false,
            dead(),
            Err(color_eyre::eyre::eyre!("root failed")),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(format!("{err}"), "root failed");
    }

    #[test]
    fn an_unsettled_tui_run_exits_with_its_own_error_first() {
        let err = super::tui_run_exit(
            false,
            None,
            Err(color_eyre::eyre::eyre!("root failed")),
            vec!["added failed".to_string()],
        )
        .unwrap_err();
        assert_eq!(format!("{err}"), "root failed");

        let err = super::tui_run_exit(
            false,
            None,
            Ok(()),
            vec!["a: first".to_string(), "b: second".to_string()],
        )
        .unwrap_err();
        assert_eq!(format!("{err}"), "a: first; b: second");
    }

    #[test]
    fn an_unsettled_tui_run_without_errors_says_it_ended_early() {
        let err = super::tui_run_exit(false, None, Ok(()), Vec::new()).unwrap_err();
        assert_eq!(
            format!("{err}"),
            "the run ended before every package was built"
        );
    }

    #[test]
    fn a_condition_failure_is_queued_again_in_the_same_run() {
        use super::{FailureKind as K, dep_failure_retries as r};
        // Even with the rounds spent: the condition counters are the bound.
        assert!(r(K::ConditionUnmet, 5, 1));
        assert!(r(K::Other, 0, 2));
        assert!(!r(K::Other, 2, 2));
        assert!(!r(K::Exhausted, 0, 3));
        assert!(!r(K::RateLimited, 0, 3));
    }

    use super::*;

    #[test]
    fn unsafe_resolve_is_off_unless_asked_for_on_build_and_update() {
        let unsafe_resolve = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Commands::Build { unsafe_resolve, .. } => unsafe_resolve,
            Commands::Update(a) => a.unsafe_resolve,
            _ => panic!("not build or update"),
        };
        let build = ["auto-package", "build", "httpx", "--project", "home:u"];
        let update = ["auto-package", "update", "--project", "home:u"];
        assert!(!unsafe_resolve(&build));
        assert!(!unsafe_resolve(&update));
        assert!(unsafe_resolve(
            &[&build[..], &["--unsafe-resolve"]].concat()
        ));
        assert!(unsafe_resolve(
            &[&update[..], &["--unsafe-resolve"]].concat()
        ));
    }

    #[test]
    fn build_takes_min_free_gb_defaulting_to_three() {
        let min_free = |extra: &[&str]| {
            let base = ["auto-package", "build", "httpx", "--project", "home:u"];
            match Cli::try_parse_from([&base[..], extra].concat())
                .unwrap()
                .command
            {
                Commands::Build { min_free_gb, .. } => min_free_gb,
                _ => panic!("not build"),
            }
        };
        assert_eq!(min_free(&[]), 3);
        assert_eq!(min_free(&["--min-free-gb", "0"]), 0);
    }

    #[test]
    fn build_takes_nvidia_mode_defaulting_to_none_chosen() {
        let parse = |extra: &[&str]| {
            let base = ["auto-package", "build", "httpx", "--project", "home:u"];
            Cli::try_parse_from([&base[..], extra].concat()).map(|c| match c.command {
                Commands::Build {
                    nvidia,
                    nvidia_redist,
                    ..
                } => (nvidia, nvidia_redist),
                _ => panic!("not build"),
            })
        };
        assert_eq!(parse(&[]).unwrap(), (None, false));
        assert_eq!(
            parse(&["--nvidia", "redist"]).unwrap(),
            (Some(NvidiaMode::Redist), false)
        );
        assert_eq!(
            parse(&["--nvidia", "wheel"]).unwrap(),
            (Some(NvidiaMode::Wheel), false)
        );
        assert_eq!(parse(&["--nvidia-redist"]).unwrap(), (None, true));
        assert!(parse(&["--nvidia", "rpm"]).is_err());
        assert!(parse(&["--nvidia", "wheel", "--nvidia-redist"]).is_err());
    }

    #[test]
    fn the_nvidia_mode_is_explicit_then_remembered_then_wheel() {
        use NvidiaMode::*;
        assert_eq!(resolve_nvidia_mode(None, None), (Wheel, None));
        assert_eq!(resolve_nvidia_mode(None, Some(Redist)), (Redist, None));
        assert_eq!(resolve_nvidia_mode(Some(Redist), None), (Redist, None));
        assert_eq!(
            resolve_nvidia_mode(Some(Redist), Some(Redist)),
            (Redist, None)
        );
        let (mode, warning) = resolve_nvidia_mode(Some(Wheel), Some(Redist));
        assert_eq!(mode, Wheel);
        assert!(warning.unwrap().contains("switching to wheel"));
    }

    #[test]
    fn the_nvidia_mode_is_remembered_in_the_work_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_nvidia_mode(dir.path()), None);
        // A plain run in the default mode leaves nothing behind.
        write_nvidia_mode(dir.path(), NvidiaMode::Wheel, false).unwrap();
        assert!(!dir.path().join(NVIDIA_MODE_FILE).exists());
        write_nvidia_mode(dir.path(), NvidiaMode::Redist, true).unwrap();
        assert_eq!(read_nvidia_mode(dir.path()), Some(NvidiaMode::Redist));
        // Once there, it is kept up to date, including a switch back.
        write_nvidia_mode(dir.path(), NvidiaMode::Wheel, true).unwrap();
        assert_eq!(read_nvidia_mode(dir.path()), Some(NvidiaMode::Wheel));
        std::fs::write(dir.path().join(NVIDIA_MODE_FILE), "garbage").unwrap();
        assert_eq!(read_nvidia_mode(dir.path()), None);
    }

    #[test]
    fn a_running_or_finished_package_is_not_started_again() {
        use ap_core::event::SelectionReason;
        let target = |name: &str| ap_build::pipeline::TargetInfo {
            name: name.to_string(),
            version: "1.0".to_string(),
            reason: SelectionReason::FirstUnbuiltDependency,
            blocked_by: vec![],
        };
        let targets = vec![target("alpha"), target("beta"), target("gamma")];
        let in_flight = vec![ap_build::osc::rpm_name("beta")];
        let done: std::collections::HashSet<String> =
            [ap_build::osc::rpm_name("gamma")].into_iter().collect();
        let fresh: Vec<String> = unstarted_targets(targets, &in_flight, &done)
            .into_iter()
            .map(|(t, _)| t.name)
            .collect();
        assert_eq!(fresh, vec!["alpha".to_string()]);
    }

    #[test]
    fn a_package_is_run_under_its_lowercase_name_whatever_obs_lists() {
        use ap_core::event::SelectionReason;
        // OBS lists the package as `python-APScheduler`. The run key is the
        // lowercase guess, so the state directory, the TUI row and the
        // in-flight check all agree on one name.
        assert_eq!(run_name("APScheduler"), "python-apscheduler");
        assert_eq!(run_name("apscheduler"), "python-apscheduler");
        let target = || ap_build::pipeline::TargetInfo {
            name: "APScheduler".to_string(),
            version: "1.0".to_string(),
            reason: SelectionReason::FirstUnbuiltDependency,
            blocked_by: vec![],
        };
        let running = vec!["python-apscheduler".to_string()];
        let fresh = unstarted_targets(vec![target()], &running, &Default::default());
        assert!(fresh.is_empty(), "a running package is not started again");
        let fresh = unstarted_targets(vec![target()], &[], &Default::default());
        assert_eq!(fresh[0].1, "python-apscheduler");
        assert!(!fresh[0].1.contains("APScheduler"));
    }

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
mod dep_drops_tests {
    use super::*;

    /// Keeps the warnings a drop reports.
    #[derive(Default)]
    struct Warnings(Mutex<Vec<String>>);

    impl Reporter for Warnings {
        fn report(&self, event: &Event) {
            if let Event::Warning { message, .. } = event {
                self.0.lock().unwrap().push(message.clone());
            }
        }
    }

    /// A dependency dropped for a concrete reason says so in a warning, and the
    /// message that later gives up on a package that needs it repeats that
    /// reason instead of pointing at output that never gave one.
    #[test]
    fn a_dropped_dependency_is_reported_and_explained_when_giving_up() {
        let reporter = Warnings::default();
        let drops = DepDrops::default();
        drops.dropped(
            &reporter,
            "vllm-flash-attn",
            "it is unresolvable: no PyPI release and no usable public source repository",
        );
        let warnings = reporter.0.lock().unwrap().clone();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("vllm-flash-attn"), "{warnings:?}");
        assert!(warnings[0].contains("no PyPI release"), "{warnings:?}");

        let why = drops.explain(&["python-vllm_flash_attn".to_string()]);
        assert!(why.contains("python-vllm_flash_attn"), "{why}");
        assert!(why.contains("no PyPI release"), "{why}");
    }

    /// The reason a dependency was dropped is the first one; seeing it dropped
    /// again as "already attempted" quotes it rather than replacing it, and the
    /// outcome of a build replaces a reason because it is newer evidence.
    #[test]
    fn the_first_reason_is_kept_and_a_build_outcome_replaces_it() {
        let reporter = Warnings::default();
        let drops = DepDrops::default();
        drops.dropped(&reporter, "foo", "the distro already provides it");
        drops.dropped(&reporter, "foo", "it was already attempted this run");
        assert_eq!(
            drops.reason("foo").as_deref(),
            Some("the distro already provides it")
        );
        // Every drop is reported, even a repeat.
        assert_eq!(reporter.0.lock().unwrap().len(), 2);

        drops.note_outcome("python-foo", "its build failed (boom)");
        assert_eq!(
            drops.reason("foo").as_deref(),
            Some("its build failed (boom)")
        );
    }

    #[test]
    fn a_dependency_with_nothing_recorded_is_still_named() {
        let drops = DepDrops::default();
        let why = drops.explain(&["mystery".to_string(), "other".to_string()]);
        assert!(why.contains("mystery") && why.contains("other"), "{why}");
        assert!(why.contains("nothing was recorded"), "{why}");
        assert_eq!(why.lines().count(), 2);
    }

    /// The vllm run's three given-up packages: the message names each with
    /// the reason its dependency is out of reach, and says nothing about
    /// "failed attempts" that never happened.
    #[test]
    fn the_give_up_message_states_each_packages_blocking_reason() {
        let reporter = Warnings::default();
        let drops = DepDrops::default();
        drops.dropped(
            &reporter,
            "fastapi-cloud-cli",
            "it is already in home:dspinella:open-webui as python-fastapi-cloud-cli with \
             status \"succeeded\", which counts as resolvable",
        );
        drops.dropped(
            &reporter,
            "cuda-bindings",
            "it is already in home:dspinella:open-webui as python-cuda-bindings with status \
             \"succeeded\", which counts as resolvable, but python-cuda-bindings is built \
             in home:dspinella:open-webui, but its spec there is an unfinished skeleton",
        );
        drops.dropped(
            &reporter,
            "nvidia-cutlass-dsl-libs-base",
            "python-nvidia-cutlass-dsl-libs-base is excluded for openSUSE_Tumbleweed/i586",
        );
        let mut give_ups = std::collections::BTreeMap::new();
        for (pkg, dep) in [
            ("python-fastapi-cli", "fastapi-cloud-cli"),
            ("python-humming-kernels", "cuda-bindings"),
            (
                "python-nvidia-cutlass-dsl-libs-cu12",
                "nvidia-cutlass-dsl-libs-base",
            ),
        ] {
            let deps = [dep.to_string()];
            give_ups.insert(
                pkg.to_string(),
                format!(
                    "it needs {dep} and none of them can be queued again:\n{}",
                    drops.explain(&deps)
                ),
            );
        }
        let msg = build_incomplete_message(&give_ups);
        assert!(
            msg.starts_with("build incomplete — gave up on 3 package(s)"),
            "{msg}"
        );
        assert!(!msg.contains("failed attempt"), "{msg}");
        for needle in [
            "- python-fastapi-cli: it needs fastapi-cloud-cli",
            "- python-humming-kernels: it needs cuda-bindings",
            "- python-nvidia-cutlass-dsl-libs-cu12: it needs nvidia-cutlass-dsl-libs-base",
            "status \"succeeded\", which counts as resolvable",
            "unfinished skeleton",
            "is excluded for openSUSE_Tumbleweed/i586",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in:\n{msg}");
        }
        // A multi-line reason stays under its package.
        let blocks: Vec<&str> = msg.split("\n  - ").skip(1).collect();
        assert_eq!(blocks.len(), 3, "{msg}");
        assert!(blocks[0].contains("\n    - fastapi-cloud-cli:"), "{msg}");
    }

    #[test]
    fn a_failure_reason_is_reported_as_given() {
        let give_ups = std::collections::BTreeMap::from([(
            "python-foo".to_string(),
            "it failed 10 times in a row (last: spec invalid)".to_string(),
        )]);
        let msg = build_incomplete_message(&give_ups);
        assert!(
            msg.ends_with("- python-foo: it failed 10 times in a row (last: spec invalid)"),
            "{msg}"
        );
    }
}

#[cfg(test)]
mod blocked_summary_tests {
    use super::*;

    #[test]
    fn a_dependency_that_failed_reads_failed_in_the_waiting_list() {
        let torch = "python-torch".to_string();
        let deps = ["torch".to_string(), "numpy".to_string()];
        assert_eq!(waiting_list(&deps, &[&torch]), "failed torch, numpy");
        assert_eq!(waiting_list(&deps, &[]), "torch, numpy");
    }

    #[test]
    fn the_summary_lists_the_rpmlint_errors_of_the_latest_build_only() {
        let root = tempfile::tempdir().unwrap();
        let ws = ap_build::workspace::Workspace::open(root.path(), "python-x").unwrap();
        std::fs::write(
            ws.dir().join("rpmlint.log"),
            "=== rpmlint of python-x 1 (unix time 1) ===\nx.rpm: E: old-error a\n\n\
             === rpmlint of python-x 2 (unix time 2) ===\nx.rpm: W: warn\nx.rpm: E: new-error b\n",
        )
        .unwrap();
        let pkg = "python-x".to_string();
        assert_eq!(
            rpmlint_errors_of(root.path(), &[&pkg]),
            ["python-x: x.rpm: E: new-error b"]
        );
    }

    #[test]
    fn the_summary_lists_every_flavors_rpmlint_errors_of_the_latest_run() {
        let root = tempfile::tempdir().unwrap();
        let ws = ap_build::workspace::Workspace::open(root.path(), "python-x").unwrap();
        std::fs::write(
            ws.dir().join("rpmlint.log"),
            "=== rpmlint of python-x 1 (unix time 1) ===\nx.rpm: E: old-error a\n\n\
             === rpmlint of python-x 2 (unix time 2) ===\nx.rpm: E: base-error\n\n\
             === rpmlint of python-x:mpi 2 (unix time 2) ===\nm.rpm: E: mpi-error\n",
        )
        .unwrap();
        let pkg = "python-x".to_string();
        assert_eq!(
            rpmlint_errors_of(root.path(), &[&pkg]),
            [
                "python-x: x.rpm: E: base-error",
                "python-x: m.rpm: E: mpi-error"
            ]
        );
    }

    #[test]
    fn a_spent_ladder_is_an_ordinary_failure() {
        let spent = PipelineResult::Failed {
            rpm_name: "python-torch".into(),
            reason: format!("{}x", ap_build::pipeline::LADDER_FAILED_PREFIX),
            kind: FailureKind::Exhausted,
        };
        assert!(matches!(
            package_outcome(&spent),
            PackageOutcome::Failed { .. }
        ));
        let failed = PipelineResult::Failed {
            rpm_name: "python-x".into(),
            reason: "no".into(),
            kind: FailureKind::Other,
        };
        assert!(matches!(
            package_outcome(&failed),
            PackageOutcome::Failed { .. }
        ));
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

#[cfg(test)]
mod settle_tests {
    use super::*;

    fn fin(pkg: &str, outcome: PackageOutcome) -> Event {
        Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "1.0".to_string(),
            outcome,
        }
    }

    fn failed(label: &str, after: usize) -> AddFailure {
        AddFailure {
            label: label.to_string(),
            message: format!("{label}: boom"),
            after,
        }
    }

    // A dependency row is started without a request name: nothing is noted
    // for it unless the dependency queue notes one (see `spawn_dep_build`).
    fn no_origin(_row: &str) -> Option<String> {
        None
    }

    fn labels(failures: &[AddFailure]) -> Vec<&str> {
        failures.iter().map(|f| f.label.as_str()).collect()
    }

    #[test]
    fn finished_since_only_looks_past_the_mark() {
        let events = vec![
            fin("python-a", PackageOutcome::Built),
            fin("python-b", PackageOutcome::Failed { reason: "x".into() }),
        ];
        assert!(finished_since(&events, 0, "python-a"));
        assert!(!finished_since(&events, 1, "python-a"));
        assert!(finished_since(&events, 1, "python-b"));
        assert!(!finished_since(&events, 2, "python-b"));
        assert!(!finished_since(&events, 0, "python-c"));
    }

    #[test]
    fn failure_is_withdrawn_when_its_package_is_later_built_as_a_dependency() {
        // The request `foo` fails, then `python-foo` builds as a dependency.
        let events = vec![
            fin("python-foo", PackageOutcome::Failed { reason: "x".into() }),
            fin("python-foo", PackageOutcome::Built),
        ];
        let mut failures = vec![failed("foo", 1)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert!(failures.is_empty());
    }

    #[test]
    fn a_dependency_build_settles_the_request_under_the_pypi_name_only() {
        // `foo` fails; `python-bar` builds as a dependency: `foo` stands.
        // `python-foo` builds as a dependency: `foo` is withdrawn.
        let events = vec![fin("python-bar", PackageOutcome::Built)];
        let mut failures = vec![failed("foo", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert_eq!(labels(&failures), vec!["foo"]);
        let events = vec![fin("python-foo", PackageOutcome::Built)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert!(failures.is_empty());
    }

    #[test]
    fn a_recorded_origin_names_the_row_over_the_rpm_name_guess() {
        // The row was requested as `baz`; a failure of `foo` is not its.
        let origin = |row: &str| (row == "python-foo").then(|| "baz".to_string());
        let events = vec![fin("python-foo", PackageOutcome::Built)];
        let mut failures = vec![failed("foo", 0), failed("baz", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), origin);
        assert_eq!(labels(&failures), vec!["foo"]);
    }

    #[test]
    fn a_dependency_label_settles_a_request_only_under_its_own_name() {
        // `python-foo` was built as the dependency `foo`: it settles a failed
        // request `foo`. `python-bar` settles nothing of `foo`.
        let label = |row: &str| (row == "python-foo").then(|| "foo".to_string());
        let events = vec![fin("python-bar", PackageOutcome::Built)];
        let mut failures = vec![failed("foo", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), label);
        assert_eq!(labels(&failures), vec!["foo"]);
        let events = vec![fin("python-foo", PackageOutcome::Built)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), label);
        assert!(failures.is_empty());
    }

    #[test]
    fn a_retry_resends_only_the_name_its_row_was_requested_with() {
        let origin = |row: &str| (row == "python-foo").then(|| "foo".to_string());
        // A new request names itself.
        let (raw, retry_of) = request_names(tui::AddRequest::New("bar".into()), no_origin);
        assert_eq!((raw.as_deref(), retry_of), (Some("bar"), None));
        // A retry of a root row resends its request name.
        let (raw, retry_of) = request_names(tui::AddRequest::Retry("python-foo".into()), origin);
        assert_eq!(
            (raw.as_deref(), retry_of.as_deref()),
            (Some("foo"), Some("python-foo"))
        );
        // A retry of a dependency row has no name to resend: it builds nothing.
        let (raw, retry_of) =
            request_names(tui::AddRequest::Retry("python-trantor".into()), no_origin);
        assert_eq!(raw, None);
        assert_eq!(retry_of.as_deref(), Some("python-trantor"));
    }

    #[test]
    fn failure_is_withdrawn_by_the_rpm_name_too() {
        let events = vec![fin("python-foo", PackageOutcome::AlreadyBuilt)];
        let mut failures = vec![failed("python-foo", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert!(failures.is_empty());
    }

    #[test]
    fn failure_after_the_build_stands() {
        // Built first, then the request failed: the later failure is not settled.
        let events = vec![fin("python-foo", PackageOutcome::Built)];
        let mut failures = vec![failed("foo", 1)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert_eq!(labels(&failures), vec!["foo"]);
    }

    #[test]
    fn failure_stands_when_the_package_did_not_end_built() {
        let events = vec![
            fin("python-foo", PackageOutcome::Built),
            fin("python-foo", PackageOutcome::Failed { reason: "x".into() }),
        ];
        let mut failures = vec![failed("foo", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert_eq!(labels(&failures), vec!["foo"]);
    }

    #[test]
    fn failure_of_another_package_stands() {
        // `python-bar` is the RPM name of `bar` only, so neither `foo` nor
        // `baz` is settled by it.
        let events = vec![fin("python-bar", PackageOutcome::Built)];
        let mut failures = vec![failed("foo", 0), failed("baz", 0)];
        withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
        assert_eq!(labels(&failures), vec!["foo", "baz"]);
    }
}
