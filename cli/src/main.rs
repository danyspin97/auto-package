//! Command-line surface — REDESIGN M1.

mod tui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ap_build::pipeline::{AgentBackend, PipelineConfig, PipelineResult, run_package, select_target};
use ap_core::event::{Event, RunOutcome};
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

        /// USD budget cap for agent calls.
        #[arg(long, default_value_t = 2.0)]
        budget_usd: f64,

        /// Build this specific dependency node instead of the auto-selected one.
        #[arg(long)]
        target: Option<String>,

        /// Rebuild even if state.json already shows this version as built.
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Regenerate the spec from scratch (ignores any existing spec).
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
    },
}

/// Whether a positional target is a repository URL rather than a package name.
fn looks_like_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://") || s.starts_with("git@")
}

/// The package name implied by a repository URL: its last path segment.
///
/// `https://github.com/BurntSushi/ripgrep.git` → `ripgrep`.
fn repo_package_name(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url)
        .to_string()
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            name,
            version,
            project,
            work_dir,
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
        } => {
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

            // Both of these are silent-wrong-behaviour traps otherwise: a
            // mistyped level would quietly log at info, and an unsupported
            // backend would quietly run the CLI one.
            let level: LogLevel = log.parse().map_err(|e: String| color_eyre::eyre::eyre!(e))?;
            if agent_backend != "cli" {
                return Err(color_eyre::eyre::eyre!(
                    "unsupported --agent-backend '{agent_backend}'; only 'cli' is available"
                ));
            }

            if cleanup && work_dir.exists() {
                std::fs::remove_dir_all(&work_dir)?;
            }

            // Canonicalize work_dir — fixes defect (a): no relative path can reach
            // a subprocess whose cwd is the osc checkout.
            let work_dir = {
                std::fs::create_dir_all(&work_dir)?;
                work_dir.canonicalize()?
            };

            let rpms_dir = work_dir.join("rpms");
            std::fs::create_dir_all(&rpms_dir)?;
            let state_root = work_dir.join("state");
            std::fs::create_dir_all(&state_root)?;

            // Persist the project name so a mis-matched --project is caught early.
            let project_file = work_dir.join("project");
            if project_file.exists() {
                let saved = std::fs::read_to_string(&project_file)?;
                let saved = saved.trim();
                if saved != project {
                    return Err(color_eyre::eyre::eyre!(
                        "work directory was created for project '{saved}' but --project is '{project}'; \
                         use --cleanup to start fresh or pick the matching work directory"
                    ));
                }
            } else {
                std::fs::write(&project_file, &project)?;
            }

            // Source overrides: whatever is on disk, plus anything given on
            // this command line, written straight back so the answer is only
            // ever given once.
            let mut source_urls = ap_build::sources::SourceMap::load(&work_dir)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            if !source_url.is_empty() {
                for arg in &source_url {
                    let (pkg, repo) = ap_build::sources::SourceMap::parse_arg(arg, &name)
                        .map_err(|e| color_eyre::eyre::eyre!(e))?;
                    source_urls.insert(&pkg, &repo);
                }
                source_urls
                    .save(&work_dir)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
            }
            let source_urls = Arc::new(source_urls);

            let run_dir = RunDir::allocate(&work_dir)?;

            let mut sinks: Vec<Box<dyn ap_core::sink::Reporter>> = Vec::new();

            // Add TUI sink if enabled, otherwise use HumanSink
            if enable_tui {
                let tui_sink = tui::TuiSink::new()?;
                sinks.push(Box::new(tui_sink));
            } else {
                sinks.push(Box::new(HumanSink::new(level)));
            }

            // Always add JSONL logging
            sinks.push(Box::new(JsonlSink::open(&run_dir.jsonl())?));

            let memory = std::sync::Arc::new(MemorySink::new());
            sinks.push(Box::new(memory.clone()));

            let reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(FanoutReporter::new(sinks));

            {
                let memory = memory.clone();
                ctrlc::set_handler(move || {
                    let usage = memory.events().iter().fold(
                        ap_core::event::Usage::default(),
                        |mut acc, ev| {
                            if let Event::AgentReply { usage, .. } = ev {
                                acc.input_tokens += usage.input_tokens;
                                acc.output_tokens += usage.output_tokens;
                                acc.cost_usd += usage.cost_usd;
                            }
                            acc
                        },
                    );
                    eprintln!(
                        "\nInterrupted — tokens: {} in / {} out — cost: ${:.4}",
                        usage.input_tokens, usage.output_tokens, usage.cost_usd
                    );
                    std::process::exit(1);
                })
                .expect("failed to set Ctrl-C handler");
            }

            let argv: Vec<String> = std::env::args().collect();
            reporter.report(&Event::RunStarted {
                run_id: run_dir.run_id.clone(),
                argv,
                work_dir: work_dir.clone(),
            });

            let start = Instant::now();
            let result = cmd_build(
                &name,
                version.as_deref(),
                &project,
                &work_dir,
                &state_root,
                &rpms_dir,
                &distro,
                &arch,
                max_attempts,
                budget_usd,
                target.as_deref(),
                force,
                regen,
                no_agent,
                jobs,
                source_repo,
                source_urls,
                &run_dir,
                reporter.clone(),
            );

            let outcome = if result.is_ok() {
                RunOutcome::Success
            } else {
                RunOutcome::Failed
            };
            // Sum usage from all AgentReply events recorded during the run.
            let total_usage = memory.events().iter().fold(
                ap_core::event::Usage::default(),
                |mut acc, ev| {
                    if let Event::AgentReply { usage, .. } = ev {
                        acc.input_tokens += usage.input_tokens;
                        acc.output_tokens += usage.output_tokens;
                        acc.cost_usd += usage.cost_usd;
                    }
                    acc
                },
            );
            reporter.report(&Event::RunFinished {
                outcome,
                elapsed: start.elapsed(),
                usage: total_usage,
            });

            result?;
        }
    }

    Ok(())
}

/// A counting semaphore that limits how many threads run concurrently.
///
/// `None` means unlimited. Each thread acquires a slot at the start of its
/// work and releases it when done; threads block until a slot is free.
type Semaphore = Arc<(std::sync::Mutex<usize>, std::sync::Condvar)>;

fn make_semaphore(jobs: usize) -> Option<Semaphore> {
    if jobs == 0 {
        None
    } else {
        Some(Arc::new((std::sync::Mutex::new(0usize), std::sync::Condvar::new())))
    }
}

fn sem_acquire(sem: &Option<Semaphore>, limit: usize) {
    if let Some(s) = sem {
        let (lock, cvar) = &**s;
        let mut active = lock.lock().unwrap();
        while *active >= limit {
            active = cvar.wait(active).unwrap();
        }
        *active += 1;
    }
}

fn sem_release(sem: &Option<Semaphore>) {
    if let Some(s) = sem {
        let (lock, cvar) = &**s;
        *lock.lock().unwrap() -= 1;
        cvar.notify_one();
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_build(
    name: &str,
    version: Option<&str>,
    project: &str,
    work_dir: &PathBuf,
    state_root: &PathBuf,
    rpms_dir: &PathBuf,
    distro: &str,
    arch: &str,
    max_attempts: usize,
    budget_usd: f64,
    explicit_target: Option<&str>,
    force: bool,
    regen: bool,
    no_agent: bool,
    jobs: usize,
    source_repo: Option<String>,
    source_urls: Arc<ap_build::sources::SourceMap>,
    run_dir: &RunDir,
    reporter: Arc<dyn Reporter + Send + Sync>,
) -> color_eyre::Result<()> {
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
                let url = format!("https://pypi.org/pypi/{name}/json");
                let json = ureq::get(&url).call()?.into_body().read_to_string()?;
                let release = ap_ecosystem::pypi::parse_release(&json)?;
                release.version
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
                let declared = probe.project_name.clone().unwrap_or_else(|| name.to_string());
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
        work_dir: work_dir.clone(),
        state_root: state_root.clone(),
        rpms_dir: rpms_dir.clone(),
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
        source_urls,
        failed_deps: Vec::new(),
    };

    let mut config = config;
    let sem = make_semaphore(jobs);

    // Deps discovered at runtime that need to be built before the next resolve.
    let mut pending_deps: Vec<String> = Vec::new();
    // Tracks deps we have already attempted to avoid infinite retry loops.
    let mut attempted_deps: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Resolve closure, select all buildable targets, build them in parallel,
    // then repeat until the entire closure is built (or an error occurs).
    loop {
        // Before re-resolving, build any runtime-discovered deps (e.g. test-only
        // deps the agent found to be unresolvable).
        while !pending_deps.is_empty() {
            let batch = std::mem::take(&mut pending_deps);
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
                .filter(|dep| attempted_deps.insert(dep.clone()))
                .filter(|dep| {
                    // Skip deps already present in the project or any of its
                    // configured build/path projects.
                    let rpm = ap_build::osc::rpm_name(dep);
                    !ap_build::osc::get_available_packages(
                        project,
                        ap_build::proc::Emit::run(reporter.as_ref()),
                    )
                    .contains(&rpm)
                })
                .map(|dep| {
                    eprintln!("  queuing missing dependency: {} (PyPI: {})",
                              ap_build::osc::rpm_name(&dep), dep);
                    let url = format!("https://pypi.org/pypi/{dep}/json");
                    let dep_version = ureq::get(&url)
                        .call()
                        .and_then(|r| r.into_body().read_to_string().map_err(Into::into))
                        .ok()
                        .and_then(|json| ap_ecosystem::pypi::parse_release(&json).ok())
                        .map(|r| r.version);
                    let dep_version = match dep_version {
                        Some(v) => v,
                        None => return None,
                    };
                    let dep_rpm = ap_build::osc::rpm_name(&dep);
                    let dep_config = PipelineConfig {
                        pkg_name: dep.clone(),
                        rpm_name: dep_rpm.clone(),
                        version: dep_version,
                        // A dependency always comes from PyPI, never from the
                        // repository the root package was given.
                        source_repo: None,
                        subprojects: Vec::new(),
                        ..config.clone()
                    };
                    let dep_pkg_run = run_dir.pkg(&dep_rpm);
                    if dep_pkg_run.ensure().is_err() {
                        return None;
                    }
                    let r = reporter.clone();
                    let sem2 = sem.clone();
                    Some(std::thread::spawn(move || {
                        sem_acquire(&sem2, jobs);
                        let result = run_package(&dep_config, &dep_pkg_run, r.as_ref());
                        sem_release(&sem2);
                        (dep_rpm, result)
                    }))
                })
                .flatten()
                .collect();

            for handle in handles {
                let (dep_rpm, result) = handle.join().expect("dep thread panicked");
                match result {
                    PipelineResult::Built { .. } => {
                        eprintln!("built dep {dep_rpm}");
                    }
                    PipelineResult::AlreadyBuilt { .. } => {
                        eprintln!("dep {dep_rpm} already built");
                    }
                    PipelineResult::MissingDeps { deps: more, .. } => {
                        pending_deps.extend(more);
                    }
                    PipelineResult::Failed { reason, .. } => {
                        reporter.report(&Event::Warning {
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
                pkg: name.to_string(),
                version: version.clone(),
                reason: ap_core::event::SelectionReason::ExplicitOverride,
                blocked_by: vec![],
            });
            match run_package(&config, &pkg_run, reporter.as_ref()) {
                PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. } => {
                    return Ok(())
                }
                PipelineResult::MissingDeps { deps, .. } => {
                    // Only queue deps we haven't tried yet; if every dep was
                    // already attempted we cannot make progress.
                    let new_deps: Vec<String> = deps
                        .iter()
                        .filter(|d| !attempted_deps.contains(*d))
                        .cloned()
                        .collect();
                    if new_deps.is_empty() {
                        eprintln!("{rpm_name} needs: {}", deps.join(", "));
                        return Ok(());
                    }
                    pending_deps.extend(new_deps);
                    continue;
                }
                PipelineResult::Failed { reason, .. } => {
                    return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"))
                }
            }
        }

        // Select all currently buildable targets (the full frontier).
        let targets = select_target(
            name,
            explicit_target,
            pinned_version.as_deref(),
            &config,
            reporter.as_ref(),
        )
            .map_err(|e| color_eyre::eyre::eyre!(e))?;

        if targets.is_empty() {
            eprintln!("{name} and all dependencies are already built");
            return Ok(());
        }

        for t in &targets {
            reporter.report(&Event::TargetSelected {
                pkg: t.name.clone(),
                version: t.version.clone(),
                reason: t.reason.clone(),
                blocked_by: t.blocked_by.clone(),
            });
        }

        // Spawn one thread per frontier package; OBS builds run concurrently.
        let handles: Vec<_> = targets
            .into_iter()
            .map(|target_info| {
                let rpm_name = ap_build::osc::rpm_name(&target_info.name);
                let pkg_config = PipelineConfig {
                    pkg_name: target_info.name.clone(),
                    rpm_name: rpm_name.clone(),
                    version: target_info.version.clone(),
                    ..config.clone()
                };
                let pkg_run = run_dir.pkg(&rpm_name);
                pkg_run.ensure().expect("failed to create pkg run dir");
                let r = reporter.clone();
                let sem2 = sem.clone();
                std::thread::spawn(move || {
                    sem_acquire(&sem2, jobs);
                    let result = run_package(&pkg_config, &pkg_run, r.as_ref());
                    sem_release(&sem2);
                    (rpm_name, target_info.version, result)
                })
            })
            .collect();

        let mut batch_pending: Vec<String> = Vec::new();
        let mut any_failed = false;

        for handle in handles {
            let (rpm_name, version, result) = handle.join().expect("build thread panicked");
            match result {
                PipelineResult::Built { .. } => {
                    eprintln!("built {rpm_name} {version}");
                }
                PipelineResult::AlreadyBuilt { .. } => {
                    eprintln!("{rpm_name} {version} already built");
                }
                PipelineResult::MissingDeps { deps, .. } => {
                    batch_pending.extend(deps);
                }
                PipelineResult::Failed { reason, .. } => {
                    eprintln!("ERROR: {rpm_name} failed: {reason}");
                    any_failed = true;
                }
            }
        }

        if any_failed {
            return Err(color_eyre::eyre::eyre!("one or more packages failed to build"));
        }

        pending_deps.extend(batch_pending);

        // If an explicit target was requested, stop after one batch.
        if explicit_target.is_some() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            repo_package_name("https://gitlab.com/group/sub/thing"),
            "thing"
        );
    }
}
