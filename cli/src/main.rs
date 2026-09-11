//! Command-line surface.

mod attempt_logger;
mod tui;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use ap_build::agent::Usage;
use ap_build::context::Level;
use ap_ecosystem::pip::Node;
use clap::{Parser, Subcommand};
use tracing::{debug, warn};

// ── logger ────────────────────────────────────────────────────────────────────

/// A callable that receives one log line at a time.
///
/// In CLI mode the logger writes to stderr. In TUI mode it sends lines to a
/// channel so the TUI can display them without suspending.
pub(crate) type Logger = Arc<dyn Fn(&str) + Send + Sync + 'static>;

pub(crate) fn stderr_logger() -> Logger {
    Arc::new(|s: &str| eprintln!("{s}"))
}

// ── disk helpers ──────────────────────────────────────────────────────────────

pub(crate) fn marker_path(pkg_dir: &Path) -> PathBuf {
    pkg_dir.join("BUILD_OK")
}

pub(crate) fn already_built(pkg_dir: &Path, version: &str) -> bool {
    fs::read_to_string(marker_path(pkg_dir))
        .map(|v| v.trim() == version)
        .unwrap_or(false)
}

// ── logging helpers ───────────────────────────────────────────────────────────

fn log_step(log: &Logger, pkg: &str, text: &str) {
    log(&format!("[{pkg}]  {text}"));
}

fn log_result(log: &Logger, pkg: &str, text: &str) {
    log(&format!("[{pkg}]    → {text}"));
}

fn log_diff(log: &Logger, pkg: &str, before: &str, after: &str) {
    let mut changes = 0;
    let mut first_few = Vec::new();

    for d in diff::lines(before, after) {
        match d {
            diff::Result::Left(l) => {
                changes += 1;
                if changes <= 5 {
                    first_few.push(format!("- {l}"));
                }
            }
            diff::Result::Right(r) => {
                changes += 1;
                if changes <= 5 {
                    first_few.push(format!("+ {r}"));
                }
            }
            diff::Result::Both(_, _) => {}
        }
    }

    if changes == 0 {
        log(&format!("[{pkg}]    (no changes)"));
    } else if changes <= 5 {
        for line in first_few {
            log(&format!("[{pkg}]      {line}"));
        }
    } else {
        for line in first_few.iter().take(5) {
            log(&format!("[{pkg}]      {line}"));
        }
        log(&format!("[{pkg}]      ... ({} total changes, see attempt log for full diff)", changes));
    }
}

fn log_excerpt(log: &Logger, pkg: &str, excerpt: &str) {
    // Show just first and last 3 lines to keep logs scannable
    let lines: Vec<&str> = excerpt.lines().collect();
    if lines.len() <= 8 {
        for line in &lines {
            log(&format!("[{pkg}]      {line}"));
        }
    } else {
        for line in lines.iter().take(3) {
            log(&format!("[{pkg}]      {line}"));
        }
        log(&format!("[{pkg}]      ... ({} lines in attempt log)", lines.len() - 6));
        for line in lines.iter().skip(lines.len() - 3) {
            log(&format!("[{pkg}]      {line}"));
        }
    }
}

fn log_agent_call(log: &Logger, pkg: &str, turn_type: &str, confidence: &str, reasoning: &str) {
    log(&format!("[{pkg}]    → {turn_type}: {confidence} confidence"));
    log(&format!("[{pkg}]      {reasoning}"));
}

fn log_call_usage(log: &Logger, pkg: &str, usage: &Usage) {
    let cost = usage.cost_usd();
    log(&format!(
        "[{pkg}]    tokens  in={} out={}  (~${cost:.4})",
        usage.input_tokens, usage.output_tokens,
    ));
}

fn log_summary(log: &Logger, usage: &Usage) {
    log(&format!("tokens  in={} out={}", usage.input_tokens, usage.output_tokens));
    if usage.cache_creation_tokens > 0 || usage.cache_read_tokens > 0 {
        log(&format!(
            "cache   write={} read={}",
            usage.cache_creation_tokens, usage.cache_read_tokens
        ));
    }
    let cost = usage.cost_usd();
    if cost > 0.0 {
        log(&format!("cost    ~${cost:.4}"));
    }
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "auto-package", about = "Automated RPM packaging for openSUSE")]
struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a package and all its missing dependencies.
    Build {
        /// PyPI package name.
        name: String,

        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,

        /// Defaults to <work-dir>/fixes/.
        #[arg(long)]
        fixes_dir: Option<PathBuf>,

        #[arg(long, default_value = "openSUSE_Tumbleweed")]
        distro: String,

        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// OBS project to build packages under (e.g. home:you:auto-package).
        #[arg(long)]
        project: String,

        /// Send the full build log to the agent at Level::FullLog.
        /// WARNING: can exceed 34k tokens — use only when lower rungs have failed.
        #[arg(long, default_value_t = false)]
        allow_full_log: bool,

        /// Force spec regeneration even if a spec already exists for this version.
        #[arg(long, default_value_t = false)]
        regen: bool,

        /// VM type passed to `osc build --vm-type`. Chroot is faster and leaves
        /// the BUILDROOT accessible after failure; kvm gives stronger isolation.
        #[arg(long, default_value = "chroot")]
        vm_type: String,

        /// Use the agent-v2 turn loop (workspace + tool use) instead of the
        /// legacy prompt-and-replace loop. Default off; enable for evaluation.
        #[arg(long, default_value_t = false)]
        agent_v2: bool,

        /// Maximum number of packages to build concurrently (default: 1, serial).
        /// Ignored when --interactive is set.
        #[arg(long, default_value_t = 1)]
        builds: usize,

        /// Open an interactive TUI and start the build immediately in the background.
        /// The TUI shows the live log, attempts, and lets you inspect diffs.
        #[arg(long, short = 'i', default_value_t = false)]
        interactive: bool,
    },
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    // In interactive (TUI) mode, silence tracing entirely — warn!/debug! write
    // directly to stderr and corrupt the alternate-screen display.
    let interactive = matches!(&cli.command, Commands::Build { interactive: true, .. });
    let log_level = if interactive {
        "off"
    } else {
        match cli.verbose {
            0 => "warn",
            1 => "info",
            _ => "debug",
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_target(false)
        .without_time()
        .init();

    match cli.command {
        Commands::Build {
            name,
            work_dir,
            distro,
            arch,
            fixes_dir,
            project,
            allow_full_log,
            regen,
            vm_type,
            agent_v2,
            builds,
            interactive,
        } => {
            let fixes_dir = fixes_dir.unwrap_or_else(|| work_dir.join("fixes"));

            if interactive {
                // Open the TUI and immediately start the build in the background.
                tui::run(
                    &work_dir,
                    &fixes_dir,
                    &project,
                    &distro,
                    &arch,
                    allow_full_log,
                    Some(&name),
                )?;
            } else {
                let log = stderr_logger();

                // Install Ctrl-C handler that prints usage and exits cleanly.
                let total_usage: Arc<Mutex<Usage>> = Arc::new(Mutex::new(Usage::default()));
                {
                    let usage_arc = Arc::clone(&total_usage);
                    let log2 = Arc::clone(&log);
                    ctrlc::set_handler(move || {
                        log2("\ninterrupted");
                        log_summary(&log2, &usage_arc.lock().unwrap());
                        std::process::exit(130);
                    })
                    .expect("failed to set Ctrl-C handler");
                }

                cmd_build(
                    &name,
                    &work_dir,
                    &fixes_dir,
                    &project,
                    &distro,
                    &arch,
                    allow_full_log,
                    regen,
                    &vm_type,
                    agent_v2,
                    builds,
                    log,
                    total_usage,
                )?;
            }
        }
    }

    Ok(())
}

// ── build orchestration ───────────────────────────────────────────────────────

struct BuildCtx {
    work_dir: PathBuf,
    rpms_dir: PathBuf,
    fixes_dir: PathBuf,
    project: String,
    distro: String,
    arch: String,
    allow_full_log: bool,
    regen: bool,
    vm_type: String,
    agent_v2: bool,
    total_usage: Arc<Mutex<Usage>>,
    log: Logger,
}

/// Run the full build loop for `name` and its dependency closure.
///
/// `log` receives every line of output.  `total_usage` accumulates token counts
/// so the caller can display or export them — the TUI passes its own accumulator
/// so cost survives across multiple builds within one session.
///
/// Does **not** install a Ctrl-C handler; the caller is responsible for that
/// (the CLI installs one, the TUI owns its own signal handling).
pub(crate) fn cmd_build(
    name: &str,
    work_dir: &Path,
    fixes_dir: &Path,
    project: &str,
    distro: &str,
    arch: &str,
    allow_full_log: bool,
    regen: bool,
    vm_type: &str,
    agent_v2: bool,
    builds: usize,
    log: Logger,
    total_usage: Arc<Mutex<Usage>>,
) -> color_eyre::Result<Usage> {
    let jobs = builds.max(1);
    let rpms_dir = work_dir.join("rpms");
    fs::create_dir_all(&rpms_dir)?;

    log(&format!("resolving {name}..."));
    let mut nodes = match resolve_cached(name, work_dir) {
        Ok(nodes) => nodes,
        Err(e) => {
            // Check if this is a "not found" error
            if let Some(pip_err) = e.downcast_ref::<ap_ecosystem::pip::Error>() {
                if matches!(pip_err, ap_ecosystem::pip::Error::NotFound(_)) {
                    log(&format!("✗ package '{name}' not found on PyPI"));
                }
            }
            return Err(e);
        }
    };
    log(&format!("  {} package(s) in closure", nodes.len()));
    for node in &nodes {
        debug!("  {} {}", node.name, node.version);
    }

    // Outer loop: re-run the scheduler whenever new packages are discovered.
    loop {
        let n = nodes.len();
        let (mut in_degree, reverse) = build_dep_graph(&nodes);
        let mut ready: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut done_count = 0usize;
        let mut all_missing: Vec<String> = Vec::new();

        if jobs == 1 {
            while let Some(idx) = ready.pop_front() {
                let node = &nodes[idx];
                let ctx = BuildCtx {
                    work_dir: work_dir.to_path_buf(),
                    rpms_dir: rpms_dir.clone(),
                    fixes_dir: fixes_dir.to_path_buf(),
                    project: project.to_string(),
                    distro: distro.to_string(),
                    arch: arch.to_string(),
                    allow_full_log,
                    regen,
                    vm_type: vm_type.to_string(),
                    agent_v2,
                    total_usage: Arc::clone(&total_usage),
                    log: Arc::clone(&log),
                };
                match build_one(node, ctx) {
                    Ok(missing) => all_missing.extend(missing),
                    Err(e) => {
                        warn!("{} failed: {e:#}", node.name);
                        log(&format!("[{}]  ✗ {e:#}", node.name));
                    }
                }
                done_count += 1;
                for &j in &reverse[idx] {
                    in_degree[j] -= 1;
                    if in_degree[j] == 0 {
                        ready.push_back(j);
                    }
                }
            }
        } else {
            std::thread::scope(|s| {
                let (done_tx, done_rx) = mpsc::channel::<(usize, Vec<String>)>();
                let mut in_flight = 0usize;

                loop {
                    while in_flight < jobs && let Some(idx) = ready.pop_front() {
                        in_flight += 1;
                        let node = nodes[idx].clone();
                        let tx = done_tx.clone();
                        let ctx = BuildCtx {
                            work_dir: work_dir.to_path_buf(),
                            rpms_dir: rpms_dir.clone(),
                            fixes_dir: fixes_dir.to_path_buf(),
                            project: project.to_string(),
                            distro: distro.to_string(),
                            arch: arch.to_string(),
                            allow_full_log,
                            regen,
                            vm_type: vm_type.to_string(),
                            agent_v2,
                            total_usage: Arc::clone(&total_usage),
                            log: Arc::clone(&log),
                        };
                        s.spawn(move || {
                            let missing = match build_one(&node, ctx) {
                                Ok(m) => m,
                                Err(e) => {
                                    warn!("{} failed: {e:#}", node.name);
                                    vec![]
                                }
                            };
                            tx.send((idx, missing)).ok();
                        });
                    }

                    if in_flight == 0 {
                        break;
                    }

                    let (completed, missing) = done_rx.recv().unwrap();
                    in_flight -= 1;
                    done_count += 1;
                    all_missing.extend(missing);
                    for &j in &reverse[completed] {
                        in_degree[j] -= 1;
                        if in_degree[j] == 0 {
                            ready.push_back(j);
                        }
                    }
                }
            });
        }

        if done_count < n {
            warn!("{} package(s) skipped — dependency cycle", n - done_count);
        }

        all_missing.sort_unstable();
        all_missing.dedup();
        let known: HashSet<String> = nodes.iter().map(|nd| normalize(&nd.name)).collect();

        // Split: packages not yet in the graph need resolving; packages
        // already in the graph but not yet built just need the loop to
        // restart so they are processed before the package that needs them.
        let to_add: Vec<String> = all_missing
            .iter()
            .filter(|p| !known.contains(&normalize(p)))
            .cloned()
            .collect();

        let needs_retry = all_missing.iter().any(|p| {
            let norm = normalize(p);
            if !known.contains(&norm) {
                return false; // will be added below
            }
            // Already known — check whether it's actually built.
            nodes.iter().any(|nd| normalize(&nd.name) == norm && {
                let pkg_dir = work_dir.join(project).join(ap_build::osc::rpm_name(&nd.name));
                !already_built(&pkg_dir, &nd.version)
            })
        });

        if to_add.is_empty() && !needs_retry {
            break;
        }
        if needs_retry && to_add.is_empty() {
            let waiting: Vec<&str> = all_missing
                .iter()
                .filter(|p| known.contains(&normalize(p)))
                .map(String::as_str)
                .collect();
            log(&format!(
                "\nrestarting loop — waiting for {} to finish: {}",
                waiting.len(),
                waiting.join(", ")
            ));
        }

        log(&format!(
            "\ndiscovered {} new package(s): {}",
            to_add.len(),
            to_add.join(", ")
        ));
        for pkg in &to_add {
            match resolve_cached(pkg, work_dir) {
                Ok(new_nodes) => {
                    let known: HashSet<String> =
                        nodes.iter().map(|nd| normalize(&nd.name)).collect();
                    for nd in new_nodes {
                        if !known.contains(&normalize(&nd.name)) {
                            log(&format!("  + {}", nd.name));
                            nodes.push(nd);
                        }
                    }
                }
                Err(e) => {
                    // Check if this is a "not found" error
                    let is_not_found = e
                        .downcast_ref::<ap_ecosystem::pip::Error>()
                        .map_or(false, |pip_err| matches!(pip_err, ap_ecosystem::pip::Error::NotFound(_)));

                    if is_not_found {
                        log(&format!("✗ package '{pkg}' not found on PyPI — skipping"));
                    } else {
                        warn!("failed to resolve {pkg}: {e:#}");
                    }
                }
            }
        }
    }

    let usage = total_usage.lock().unwrap().clone();
    log_summary(&log, &usage);

    Ok(usage)
}

// ── notes.md helper ───────────────────────────────────────────────────────────

fn append_notes(notes_path: &Path, section: &str) {
    let mut existing = fs::read_to_string(notes_path).unwrap_or_default();
    existing.push_str(section);
    let _ = fs::write(notes_path, existing);
}

// ── agent-v2 build loop ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_one_v2(
    node: &ap_ecosystem::pip::Node,
    ctx: &BuildCtx,
    ws: &ap_build::workspace::Workspace,
    sdist_root: &Path,
    spec_path: &Path,
    spec_name: &str,
    initial_spec: &str,
    initial_pending: Option<(PathBuf, String, String, String)>,
) -> color_eyre::Result<Vec<String>> {
    use ap_build::agent::{Action, Confidence, Task, TurnBudget, TurnRequest};
    use std::time::{SystemTime, UNIX_EPOCH};

    let log = &ctx.log;
    let pkg = &node.name;
    let notes_path = ws.dir().join("notes.md");

    let mut missing_packages: Vec<String> = Vec::new();
    let mut attempts = 0usize;
    let mut spent_usd = 0.0f64;
    let mut spec_content = initial_spec.to_owned();
    let mut last_error_sig: Option<String> = None;
    const MAX_ATTEMPTS: usize = 6;

    // Symlink the sdist source root into the workspace so the agent can easily access it.
    let ws_source_link = ws.dir().join("source");
    let _ = fs::remove_file(&ws_source_link);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if let Err(e) = symlink(sdist_root, &ws_source_link) {
            log(&format!("[{pkg}]    warning: could not symlink source to workspace: {e}"));
        }
    }

    // If the initial review queued an rpmspec error, start with FixAfterValidation.
    let mut task_override: Option<Task> = if initial_pending.is_some() {
        Some(Task::FixAfterValidation)
    } else {
        None
    };

    loop {
        attempts += 1;
        if attempts > MAX_ATTEMPTS {
            log(&format!("[{pkg}]  ✗ gave up after {MAX_ATTEMPTS} attempts (v2)"));
            break;
        }

        // Determine task for this turn.
        let (task, hint) = if let Some(t) = task_override.take() {
            // Validation error from the review phase.
            let hint = if ws.rpmspec_log().exists() {
                let raw = fs::read_to_string(ws.rpmspec_log()).unwrap_or_default();
                let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                Some(ap_build::log::extract(&raw, &mapper))
            } else {
                None
            };
            (t, hint)
        } else {
            // Build step.
            log_step(log, pkg, &format!("build {attempts}/{MAX_ATTEMPTS}"));
            match ap_build::osc::build(
                &ctx.work_dir.join(&ctx.project).join(ap_build::osc::rpm_name(pkg)),
                spec_name,
                &ctx.vm_type,
                &ws.build_log(),
                &ctx.rpms_dir,
                &ctx.distro,
                &ctx.arch,
            ) {
                Ok(()) => {
                    log(&format!("[{pkg}]  ✓ built (v2)"));
                    let pkg_dir = ctx.work_dir.join(&ctx.project).join(ap_build::osc::rpm_name(pkg));
                    fs::write(marker_path(&pkg_dir), &node.version)?;
                    let _ = ws.link_buildroot(None);
                    break;
                }
                Err(ap_build::osc::Error::BuildFailed { .. }) => {
                    match ap_build::osc::find_buildroot(&ctx.distro, &ctx.arch) {
                        Some(br) => {
                            if let Err(e) = ws.link_buildroot(Some(&br)) {
                                log(&format!("[{pkg}]    warning: failed to link buildroot: {e}"));
                            } else {
                                log(&format!("[{pkg}]    buildroot → {}", br.display()));
                            }
                        }
                        None => {
                            let _ = ws.link_buildroot(None);
                            if ctx.vm_type == "kvm" {
                                log(&format!("[{pkg}]    buildroot not accessible (kvm builds don't preserve BUILDROOT)"));
                            } else {
                                log(&format!("[{pkg}]    warning: buildroot not found at /var/tmp/build-root/{}-{}/", ctx.distro, ctx.arch));
                            }
                        }
                    }
                    let log_raw = fs::read_to_string(ws.build_log()).unwrap_or_default();
                    let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                    let extracted = ap_build::log::extract(&log_raw, &mapper);
                    let section = extracted.effective_section();

                    for m in extract_missing_modules(&log_raw) {
                        if !missing_packages.iter().any(|p| normalize(p) == normalize(&m)) {
                            log(&format!("[{pkg}]    ModuleNotFoundError → queuing {m}"));
                            missing_packages.push(m);
                        }
                    }

                    log(&format!("[{pkg}]  ✗ {section} failed (v2)"));
                    if !extracted.excerpt.is_empty() {
                        log_excerpt(log, pkg, &extracted.excerpt);
                    }

                    (Task::FixAfterBuild, Some(extracted))
                }
                Err(ap_build::osc::Error::UnresolvableDeps(rpm_names)) => {
                    let pypi_names: Vec<String> = {
                        let mut set = std::collections::HashSet::new();
                        for rpm in &rpm_names {
                            if let Some(n) = pypi_name_from_rpm(rpm) {
                                set.insert(n);
                            }
                        }
                        let mut v: Vec<_> = set.into_iter().collect();
                        v.sort_unstable();
                        v
                    };
                    if pypi_names.is_empty() {
                        log(&format!("[{pkg}]  ✗ unresolvable: {}", rpm_names.join(", ")));
                    } else {
                        log(&format!("[{pkg}]  ✗ missing: {} — deferring", pypi_names.join(", ")));
                        missing_packages.extend(pypi_names);
                    }
                    break;
                }
                Err(ap_build::osc::Error::NotActionable(msg)) => {
                    log(&format!("[{pkg}]  ✗ {msg}"));
                    break;
                }
                Err(e) => {
                    log(&format!("[{pkg}]  ✗ osc error: {e}"));
                    break;
                }
            }
        };

        // Create attempt directory and start logging immediately
        let task_kind = match task {
            Task::Review => "review",
            Task::FixAfterBuild => "build",
            Task::FixAfterValidation => "rpmspec",
        };
        let section = hint.as_ref().and_then(|h| h.failed_section.as_deref()).unwrap_or("");
        let error_excerpt = hint.as_ref().map(|h| h.excerpt.as_str()).unwrap_or("");

        let mut attempt_log = match attempt_logger::AttemptLogger::start(&ctx.fixes_dir, &node.name, &attempts.to_string()) {
            Ok(l) => l,
            Err(e) => {
                log(&format!("[{pkg}]    warning: failed to create attempt log: {e}"));
                continue;
            }
        };
        attempt_log.write_kind(task_kind);
        attempt_log.write_section(section);
        attempt_log.write_error(error_excerpt);

        // Snapshot the spec before the turn.
        let spec_before = fs::read_to_string(spec_path).unwrap_or_else(|_| spec_content.clone());
        attempt_log.write_spec_before(&spec_before);

        // Run the agent turn.
        log_step(log, pkg, &format!("agent-v2 turn {attempts}/{MAX_ATTEMPTS}"));
        attempt_log.set_phase(attempt_logger::AttemptPhase::CallingAgent);

        let notes = fs::read_to_string(&notes_path).unwrap_or_default();
        let hint_ref = hint.as_ref();
        let req = TurnRequest {
            workspace: ws.dir(),
            pkg_name: pkg,
            version: &node.version,
            task,
            attempt: attempts,
            max_attempts: MAX_ATTEMPTS,
            spent_usd,
            hint: hint_ref,
            notes: &notes,
            budget: TurnBudget::default(),
        };

        match ap_build::agent::turn(req) {
            Ok(outcome) => {
                // Write prompt and response to attempt log immediately
                attempt_log.write_prompt(&outcome.prompt);

                let confidence_str = format!("{:?}", outcome.confidence);
                let action_str = format!("{:?}", outcome.action);
                attempt_log.write_response(&outcome.reasoning, &confidence_str, &action_str);
                attempt_log.write_missing_packages(&outcome.missing_packages);

                // Log the outcome concisely
                log_agent_call(log, pkg, task_kind, &confidence_str, &outcome.reasoning);

                ctx.total_usage.lock().unwrap().add(&outcome.usage);
                spent_usd += outcome.usage.cost_usd();
                log_call_usage(log, pkg, &outcome.usage);

                if !outcome.missing_packages.is_empty() {
                    log(&format!("[{pkg}]    missing: {}", outcome.missing_packages.join(", ")));
                }
                missing_packages.extend(outcome.missing_packages.iter().cloned());

                let spec_after = fs::read_to_string(spec_path).unwrap_or_else(|_| spec_before.clone());

                // Write spec_after and diff to attempt log immediately
                attempt_log.write_spec_after(&spec_after);
                attempt_log.write_diff(&spec_before, &spec_after);

                let diff_str: String = {
                    let mut d = String::new();
                    for line in diff::lines(&spec_before, &spec_after) {
                        match line {
                            diff::Result::Left(l) => { d.push_str("- "); d.push_str(l); d.push('\n'); }
                            diff::Result::Right(r) => { d.push_str("+ "); d.push_str(r); d.push('\n'); }
                            diff::Result::Both(_, _) => {}
                        }
                    }
                    d
                };

                // Determine error signature for identical-error detection.
                let sig = hint_ref.map(|h| h.excerpt.as_str()).unwrap_or("").to_owned();
                let sig_repeated = last_error_sig.as_deref() == Some(sig.as_str()) && !sig.is_empty();
                last_error_sig = if sig.is_empty() { last_error_sig } else { Some(sig) };

                // After an edit, validate with rpmspec --parse.
                let rpmspec_ok = if outcome.action == Action::Edited {
                    attempt_log.set_phase(attempt_logger::AttemptPhase::Validating);
                    log_diff(log, pkg, &spec_before, &spec_after);
                    match ap_build::osc::rpmspec_check(spec_path) {
                        Ok(()) => true,
                        Err(e) => {
                            log(&format!("[{pkg}]    rpmspec: {e} — reverting"));
                            fs::write(spec_path, &spec_before)?;
                            let _ = fs::write(ws.rpmspec_log(), format!("{e}"));
                            false
                        }
                    }
                } else {
                    true
                };

                // Build the notes.md entry.
                let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let now = format!("{}s", now);
                let result_line = match &outcome.action {
                    Action::Edited if rpmspec_ok => {
                        spec_content = spec_after.clone();
                        if sig_repeated {
                            format!("Result: spec edited (rpmspec ok). Error signature IDENTICAL to prior attempt.\n  Your model of this failure may be wrong. Investigate before editing again.")
                        } else {
                            format!("Result: spec edited (rpmspec ok). Will build next.")
                        }
                    }
                    Action::Edited => {
                        format!("Result: edit reverted — rpmspec --parse failed. Will retry with FixAfterValidation.")
                    }
                    Action::NoChange => {
                        format!("Result: no change. Will build as-is.")
                    }
                    Action::Blocked { why } => {
                        format!("Result: blocked — {why}")
                    }
                };

                let confidence_str = format!("{:?}", outcome.confidence).to_lowercase();
                let notes_entry = format!(
                    "\n## Attempt {attempts} — {task:?} — {now} — ${spent:.2} — {} tool calls\n\
                     Reasoning: {}\n\
                     Confidence: {confidence_str}\n\
                     {}\
                     {result_line}\n",
                    outcome.tool_calls,
                    outcome.reasoning,
                    if diff_str.is_empty() { String::new() } else { format!("Diff:\n{diff_str}") },
                    task = task,
                    now = now,
                    spent = outcome.usage.cost_usd(),
                );
                append_notes(&notes_path, &notes_entry);

                // Mark attempt as complete
                attempt_log.complete();

                match outcome.action {
                    Action::Blocked { why } => {
                        log(&format!("[{pkg}]    blocked: {why}"));
                        break;
                    }
                    Action::Edited if !rpmspec_ok => {
                        // Treat as FixAfterValidation next turn.
                        task_override = Some(Task::FixAfterValidation);
                    }
                    _ => {}
                }

                if outcome.confidence == Confidence::Low && outcome.action == Action::NoChange {
                    log(&format!("[{pkg}]    low confidence, no change — giving up"));
                    break;
                }
            }
            Err(e) => {
                log(&format!("[{pkg}]    agent-v2 error: {e}"));
                attempt_log.fail(&e.to_string());
                break;
            }
        }
    }

    missing_packages.sort_unstable();
    missing_packages.dedup();
    Ok(missing_packages)
}

fn build_one(node: &Node, ctx: BuildCtx) -> color_eyre::Result<Vec<String>> {
    let log = &ctx.log;
    let pkg = &node.name;
    let rpm_pkg = ap_build::osc::rpm_name(pkg);
    let pkg_dir = ctx.work_dir.join(&ctx.project).join(&rpm_pkg);
    let state_root = ctx.work_dir.join("state");
    let ws = ap_build::workspace::Workspace::open(&state_root, &rpm_pkg)?;

    log(&format!("\n[{pkg} {}]", node.version));

    // Always ensure package exists and run checkout.
    // checkout() is idempotent (uses `osc up` when .osc exists) and handles
    // packages created locally but not yet server-backed (404 errors).
    log_step(log, pkg, &format!("setting up {}/{rpm_pkg}", ctx.project));
    ap_build::osc::ensure_package(&ctx.project, &rpm_pkg, &ctx.work_dir)?;
    ap_build::osc::checkout(&ctx.project, &rpm_pkg, &ctx.work_dir)?;
    log_result(log, pkg, pkg_dir.to_str().unwrap_or("?"));

    if already_built(&pkg_dir, &node.version) {
        log(&format!("[{pkg}]  already built {}", node.version));
        return Ok(vec![]);
    }

    let spec_name = format!("python-{}.spec", node.name.to_lowercase());
    let spec_path = pkg_dir.join(&spec_name);
    let need_spec = ctx.regen
        || !spec_path.exists()
        || spec_version_differs(&spec_path, &node.version);

    // Keep the workspace spec symlink fresh. Use the canonical absolute path so
    // the symlink remains valid from any working directory.
    let abs_spec = spec_path.canonicalize().unwrap_or_else(|_| spec_path.clone());
    let _ = ws.link_spec(&abs_spec);

    let expected_tarball = pkg_dir.join(format!(
        "{}-{}.tar.gz",
        node.name.to_lowercase(),
        node.version
    ));
    let tarball_cached = expected_tarball.exists();
    let need_pypi = !tarball_cached || need_spec;

    let (tarball, release) = if need_pypi {
        let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
        let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
        let release = ap_ecosystem::pypi::parse_release(&json)?;
        debug!("[{pkg}] license: {:?}", release.license);

        if release.yanked {
            log(&format!("[{pkg}]  ✗ yanked — skipping"));
            return Ok(vec![]);
        }

        let tarball = if tarball_cached {
            expected_tarball
        } else {
            log_step(log, pkg, &format!("downloading {}", release.sdist.filename));
            release.sdist.fetch(&pkg_dir)?
        };

        (tarball, Some(release))
    } else {
        (expected_tarball, None)
    };

    let (mut spec_content, sdist_files) = if need_spec {
        let release = if let Some(r) = release {
            r
        } else {
            let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
            let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
            let r = ap_ecosystem::pypi::parse_release(&json)?;
            if r.yanked {
                log(&format!("[{pkg}]  ✗ yanked — skipping"));
                return Ok(vec![]);
            }
            r
        };

        let sdist = ap_ecosystem::sdist::inspect(&tarball)?;
        debug!("[{pkg}] build-backend: {:?}", sdist.build_system.backend);
        debug!(
            "[{pkg}] has_c={} has_tests={}",
            sdist.has_c_extensions, sdist.has_tests
        );

        log_step(log, pkg, "generating spec");
        let raw = ap_build::spec::generate(&release, &sdist.build_system)?;
        let mut content = ap_build::spec::post_process_with_tests(
            &raw,
            &release,
            &sdist.build_system,
            sdist.has_tests,
        );
        content = ap_build::spec::normalize_python_module_versions(&content);

        if sdist.has_c_extensions {
            content = ap_build::spec::fix_c_extension_arch(&content);
        }
        content = ap_build::spec::fix_files_case(&content, &release.name);
        if let Some(top) = &sdist.top_level_dir {
            if let Some(fixed) = ap_build::spec::fix_autosetup_dir(&content, top) {
                content = fixed;
            }
        }

        let mut notes = vec![format!(
            "backend: {}",
            sdist.build_system.backend.as_deref().unwrap_or("setuptools")
        )];
        if !sdist.has_tests {
            notes.push("tests: none found in sdist — %check omitted by policy".to_string());
        }
        if sdist.has_c_extensions {
            notes.push("arch: C extensions detected — arch-specific build".to_string());
        }
        content = ap_build::spec::prepend_generation_notes(&content, &notes);

        fs::write(&spec_path, &content)?;
        // Keep a pristine copy in the workspace for diff and rollback.
        let _ = fs::write(ws.dir().join("spec.generated"), &content);
        // Also write it next to the spec for human convenience.
        let _ = fs::write(pkg_dir.join("spec.generated"), &content);
        // Clear any prior review marker since the spec changed.
        let _ = fs::remove_file(ws.dir().join(".reviewed"));
        log_result(log, pkg, spec_path.to_str().unwrap_or("?"));
        (content, Some(sdist.files))
    } else {
        debug!("[{pkg}] spec already present");
        // Populate sdist_files for review even without regenerating by reading tarball.
        let files = if let Some(r) = release {
            let sdist = ap_ecosystem::sdist::inspect(&tarball)?;
            let _ = r; // release consumed for sdist info only
            Some(sdist.files)
        } else {
            None
        };
        (fs::read_to_string(&spec_path)?, files)
    };

    // Extract the sdist into the workspace sdist/ directory so it persists
    // across runs and process restarts.
    let sdist_dir = ws.sdist_dir();
    let sdist_root = if ws.sdist_extracted() {
        // Re-use previously extracted tree.
        std::fs::read_dir(&sdist_dir)
            .ok()
            .and_then(|mut e| {
                e.find_map(|entry| {
                    let entry = entry.ok()?;
                    if entry.file_type().ok()?.is_dir() {
                        Some(entry.path())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| sdist_dir.clone())
    } else {
        std::fs::create_dir_all(&sdist_dir)?;
        ap_ecosystem::sdist::extract(&tarball, &sdist_dir)?;
        std::fs::read_dir(&sdist_dir)
            .ok()
            .and_then(|mut e| {
                e.find_map(|entry| {
                    let entry = entry.ok()?;
                    if entry.file_type().ok()?.is_dir() {
                        Some(entry.path())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| sdist_dir.clone())
    };

    // pending_log: (log_path, fix_kind, fix_section, description_for_log).
    // Set to bypass the build step and call the agent directly (e.g. after an
    // rpmspec syntax error or a low-confidence escalation).
    let reviewed_marker = ws.dir().join(".reviewed");
    let needs_review = !reviewed_marker.exists();
    let mut pending_log: Option<(PathBuf, String, String, String)> = if needs_review {
        match ap_build::osc::rpmspec_check(&spec_path) {
            Ok(()) => {
                log_step(log, pkg, "review");

                // Create review attempt log immediately
                let mut review_log = attempt_logger::AttemptLogger::start(&ctx.fixes_dir, &node.name, "review").ok();
                if let Some(log) = &mut review_log {
                    log.write_kind("review");
                    log.write_spec_before(&spec_content);
                }

                match ap_build::agent::review(&spec_path, pkg, sdist_files.as_deref().unwrap_or(&[])) {
                    Ok(Some((reviewed, usage, review_prompt, review_reasoning))) => {
                        // Write to attempt log immediately
                        if let Some(log) = &mut review_log {
                            log.write_prompt(&review_prompt);
                            log.write_response(&review_reasoning, "High", "Edited");
                            log.write_spec_after(&reviewed);
                            log.write_diff(&spec_content, &reviewed);
                        }

                        // Log the review outcome concisely
                        log_agent_call(log, pkg, "review", "High", &review_reasoning);

                        ctx.total_usage.lock().unwrap().add(&usage);
                        log_call_usage(log, pkg, &usage);
                        log_diff(log, pkg, &spec_content, &reviewed);
                        fs::write(&spec_path, &reviewed)?;
                        // Verify the review didn't introduce a syntax error.
                        match ap_build::osc::rpmspec_check(&spec_path) {
                            Ok(()) => {
                                spec_content = reviewed;
                                log_result(log, pkg, "review applied fix");
                                if let Some(log) = review_log {
                                    log.complete();
                                }
                            }
                            Err(e) => {
                                log(&format!("[{pkg}]    review introduced syntax error — reverting: {e}"));
                                fs::write(&spec_path, &spec_content)?;
                                if let Some(log) = review_log {
                                    log.fail(&format!("rpmspec check failed: {e}"));
                                }
                            }
                        }
                        let _ = fs::write(&reviewed_marker, "");
                    }
                    Ok(None) => {
                        log_result(log, pkg, "review: no issues");
                        let _ = fs::write(&reviewed_marker, "");
                        // No attempt log created for "no issues"
                    }
                    Err(e) => {
                        log(&format!("[{pkg}]    review error: {e}"));
                        if let Some(log) = review_log {
                            log.fail(&e.to_string());
                        }
                    }
                }
                None
            }
            Err(e) => {
                log_step(log, pkg, "rpmspec (generated spec)");
                log(&format!("[{pkg}]    rpmspec: {e}"));
                let rpmspec_log = ws.rpmspec_log();
                // Write just the stderr so the log extractor gets clean rpmspec output.
                fs::write(&rpmspec_log, format!("{e}"))?;
                Some((rpmspec_log, "rpmspec".to_string(), String::new(), "rpmspec error".to_string()))
            }
        }
    } else {
        None
    };

    // ── agent-v2 loop ─────────────────────────────────────────────────────────
    if ctx.agent_v2 {
        return build_one_v2(
            node, &ctx, &ws, &sdist_root, &spec_path, &spec_name, &spec_content, pending_log,
        );
    }

    // ── legacy loop ───────────────────────────────────────────────────────────
    let mut history: Vec<ap_build::agent::Turn> = Vec::new();
    let mut missing_packages: Vec<String> = Vec::new();
    let mut attempts = 0;
    let mut fix_count = 1usize;
    let mut test_fix_attempts = 0usize;
    let mut rpmspec_failures = 0usize;
    let mut last_was_check = false;
    const MAX_BUILD_ATTEMPTS: usize = 6;
    const MAX_TEST_FIX_ATTEMPTS: usize = 2;
    const MAX_RPMSPEC_FAILURES: usize = 3;

    let mut escalation = ap_build::context::Escalation::new(false);
    let mut last_error_sig: Option<String> = None;

    loop {
        // Returns (log_path, kind, section, escalation_level).
        // kind: "rpmspec" | "build" | "test"
        let (build_log_path, fix_kind, fix_section, _fix_escalation) =
            if let Some((p, k, s, desc)) = pending_log.take() {
                log_step(log, pkg, &format!("agent ({desc})"));
                attempts += 1;
                (p, k, s, escalation.level().to_string())
            } else {
                log_step(log, pkg, &format!("build {}/{MAX_BUILD_ATTEMPTS}", attempts + 1));

                match ap_build::osc::build(&pkg_dir, &spec_name, &ctx.vm_type, &ws.build_log(), &ctx.rpms_dir, &ctx.distro, &ctx.arch) {
                    Ok(()) => {
                        log(&format!("[{pkg}]  ✓ built"));
                        fs::write(marker_path(&pkg_dir), &node.version)?;
                        // Clear stale buildroot symlink on success.
                        let _ = ws.link_buildroot(None);
                        break;
                    }
                    Err(ap_build::osc::Error::BuildFailed { log: build_log, .. }) => {
                        // Symlink the BUILDROOT into the workspace so the agent can inspect it.
                        match ap_build::osc::find_buildroot(&ctx.distro, &ctx.arch) {
                            Some(br) => {
                                if let Err(e) = ws.link_buildroot(Some(&br)) {
                                    log(&format!("[{pkg}]    warning: failed to link buildroot: {e}"));
                                } else {
                                    log(&format!("[{pkg}]    buildroot → {}", br.display()));
                                }
                            }
                            None => {
                                let _ = ws.link_buildroot(None);
                                if ctx.vm_type == "kvm" {
                                    log(&format!("[{pkg}]    buildroot not accessible (kvm builds don't preserve BUILDROOT)"));
                                } else {
                                    log(&format!("[{pkg}]    warning: buildroot not found at /var/tmp/build-root/{}-{}/", ctx.distro, ctx.arch));
                                }
                            }
                        }
                        let log_raw = fs::read_to_string(&build_log).unwrap_or_default();
                        let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                        let extracted = ap_build::log::extract(&log_raw, &mapper);

                        let section_str = extracted.effective_section().to_owned();
                        let is_check = section_str == "%check";
                        last_was_check = is_check;

                        if !is_check || test_fix_attempts >= MAX_TEST_FIX_ATTEMPTS {
                            attempts += 1;
                        }
                        if is_check && test_fix_attempts < MAX_TEST_FIX_ATTEMPTS {
                            test_fix_attempts += 1;
                        }

                        let sig = extracted.excerpt.clone();
                        if last_error_sig.as_deref() == Some(sig.as_str()) {
                            if escalation.level() < Level::FailingFiles {
                                let _ = escalation
                                    .escalate(ap_build::context::EscalationReason::RepeatedSignature);
                                log(&format!(
                                    "[{pkg}]  same error repeated → escalating to {}",
                                    escalation.level()
                                ));
                            } else {
                                log(&format!(
                                    "[{pkg}]  same error at {} — retrying",
                                    escalation.level()
                                ));
                            }
                        } else {
                            // New error signature: reset both escalation and history.
                            escalation = ap_build::context::Escalation::new(true);
                            history = Vec::new();
                        }
                        last_error_sig = Some(sig);

                        for m in extract_missing_modules(&log_raw) {
                            if !missing_packages.iter().any(|p| normalize(p) == normalize(&m)) {
                                log(&format!("[{pkg}]    ModuleNotFoundError → queuing {m}"));
                                missing_packages.push(m);
                            }
                        }

                        log(&format!("[{pkg}]  ✗ {section_str} failed"));
                        for line in &extracted.error_lines {
                            let display = if let Some(after) = line
                                .split("/BUILDROOT/")
                                .nth(1)
                                .and_then(|s| s.split_once('/').map(|(_, rest)| rest))
                            {
                                std::borrow::Cow::Owned(format!(
                                    "{} {}",
                                    line.split_whitespace().next().unwrap_or("error:"),
                                    after
                                ))
                            } else {
                                std::borrow::Cow::Borrowed(line.as_str())
                            };
                            log(&format!("[{pkg}]    {display}"));
                        }

                        if !extracted.excerpt.is_empty() {
                            log_excerpt(log, pkg, &extracted.excerpt);
                        } else {
                            log(&format!(
                                "[{pkg}]    (extractor found nothing — last 20 log lines)"
                            ));
                            let tail: Vec<&str> = log_raw.lines().rev().take(20).collect();
                            log_excerpt(
                                log,
                                pkg,
                                &tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
                            );
                        }

                        if attempts >= MAX_BUILD_ATTEMPTS {
                            log(&format!("[{pkg}]  ✗ gave up after {attempts} attempts"));
                            break;
                        }

                        let kind = if is_check { "test" } else { "build" }.to_string();
                        let esc = format!("{}", escalation.level());
                        log_step(log, pkg, &format!("agent ({})", escalation.level()));
                        (build_log, kind, section_str, esc)
                    }
                    Err(ap_build::osc::Error::UnresolvableDeps(rpm_names)) => {
                        let pypi_names: Vec<String> = {
                            let mut set = std::collections::HashSet::new();
                            for rpm in &rpm_names {
                                if let Some(name) = pypi_name_from_rpm(rpm) {
                                    set.insert(name);
                                }
                            }
                            let mut v: Vec<_> = set.into_iter().collect();
                            v.sort_unstable();
                            v
                        };
                        if pypi_names.is_empty() {
                            log(&format!(
                                "[{pkg}]  ✗ unresolvable: {} — cannot determine PyPI names",
                                rpm_names.join(", ")
                            ));
                        } else {
                            log(&format!(
                                "[{pkg}]  ✗ missing: {} — deferring until built",
                                pypi_names.join(", ")
                            ));
                            missing_packages.extend(pypi_names);
                        }
                        break;
                    }
                    Err(ap_build::osc::Error::NotActionable(msg)) => {
                        log(&format!("[{pkg}]  ✗ {msg}"));
                        break;
                    }
                    Err(e) => {
                        log(&format!("[{pkg}]  ✗ osc error: {e}"));
                        break;
                    }
                }
            };

        // Create attempt log immediately so TUI can see it
        let mut attempt_log = attempt_logger::AttemptLogger::start(&ctx.fixes_dir, &node.name, &fix_count.to_string()).ok();
        if let Some(log) = &mut attempt_log {
            log.write_kind(&fix_kind);
            log.write_section(&fix_section);
            log.write_escalation(&format!("{}", escalation.level()));
            log.write_spec_before(&spec_content);
        }

        let force_test_skip = last_was_check && test_fix_attempts >= MAX_TEST_FIX_ATTEMPTS;
        let section_hint = if fix_kind == "rpmspec" { Some("rpmspec-syntax") } else { None };
        match ap_build::agent::fix(
            &spec_path,
            &build_log_path,
            &node.name,
            &history,
            &escalation,
            &sdist_root,
            ctx.allow_full_log,
            force_test_skip,
            section_hint,
        ) {
            Ok((fixed, usage, new_history, confidence, new_missing)) => {
                use ap_build::agent::Confidence;

                // Write attempt data from the latest turn
                if let (Some(attempt_log), Some(last_turn)) = (&mut attempt_log, new_history.last()) {
                    attempt_log.write_error(&last_turn.error_excerpt);
                    attempt_log.write_prompt(&last_turn.prompt);
                    let confidence_str = format!("{:?}", confidence);
                    attempt_log.write_response(&last_turn.reasoning, &confidence_str, "Edited");
                    attempt_log.write_missing_packages(&new_missing);
                }

                // Log the agent outcome concisely
                if new_history.last().is_some() {
                    log_agent_call(log, pkg, &fix_kind, &format!("{:?}", confidence),
                                   &new_history.last().unwrap().reasoning);
                }

                ctx.total_usage.lock().unwrap().add(&usage);
                log_call_usage(log, pkg, &usage);
                if !new_missing.is_empty() {
                    log(&format!("[{pkg}]    missing packages: {}", new_missing.join(", ")));
                }
                missing_packages.extend(new_missing);

                // If the agent only changed comments, escalate and retry without
                // applying — a comment change cannot fix a build failure.
                if is_comment_only_diff(&spec_content, &fixed) {
                    log(&format!("[{pkg}]    agent only changed comments"));
                    history = new_history;
                    match escalation.escalate(ap_build::context::EscalationReason::ModelRequested) {
                        Ok(level) => {
                            log(&format!("[{pkg}]    escalating to {level} and retrying"));
                            pending_log = Some((
                                build_log_path.clone(),
                                fix_kind.clone(),
                                fix_section.clone(),
                                "escalated retry".to_string(),
                            ));
                            continue;
                        }
                        Err(_) => {
                            log(&format!("[{pkg}]    at max context — giving up"));
                            break;
                        }
                    }
                }

                // Fix 4: on low confidence, escalate and retry with more context
                // rather than applying a guess that might corrupt the spec.
                if confidence == Confidence::Low {
                    match escalation.escalate(ap_build::context::EscalationReason::ModelRequested) {
                        Ok(level) => {
                            log(&format!("[{pkg}]    low confidence — escalating to {level}"));
                            history = new_history;
                            pending_log = Some((
                                build_log_path.clone(),
                                fix_kind.clone(),
                                fix_section.clone(),
                                "escalated retry".to_string(),
                            ));
                            continue;
                        }
                        Err(_) => {
                            log(&format!("[{pkg}]    low confidence at max context — applying anyway"));
                        }
                    }
                }

                log_diff(log, pkg, &spec_content, &fixed);

                // Write spec_after and diff to attempt log
                if let Some(log) = &mut attempt_log {
                    log.write_spec_after(&fixed);
                    log.write_diff(&spec_content, &fixed);
                }

                fs::write(&spec_path, &fixed)?;

                if let Err(e) = ap_build::osc::rpmspec_check(&spec_path) {
                    rpmspec_failures += 1;
                    log(&format!("[{pkg}]    rpmspec: {e}"));
                    if rpmspec_failures >= MAX_RPMSPEC_FAILURES {
                        log(&format!(
                            "[{pkg}]    rpmspec failed {rpmspec_failures}× — restoring last good spec"
                        ));
                        fs::write(&spec_path, &spec_content)?;
                        if let Some(log) = attempt_log {
                            log.fail(&format!("rpmspec failed {rpmspec_failures}× times"));
                        }
                        rpmspec_failures = 0;
                        attempts += 1;
                        if attempts >= MAX_BUILD_ATTEMPTS {
                            log(&format!("[{pkg}]  ✗ gave up after {attempts} attempts"));
                            break;
                        }
                    } else {
                        let rpmspec_log = ws.rpmspec_log();
                        // Write just the rpmspec stderr for cleaner agent input.
                        fs::write(&rpmspec_log, format!("{e}"))?;
                        if let Some(log) = attempt_log {
                            log.fail(&format!("rpmspec check failed: {e}"));
                        }
                        pending_log = Some((
                            rpmspec_log,
                            "rpmspec".to_string(),
                            String::new(),
                            "rpmspec error".to_string(),
                        ));
                        spec_content = fixed;
                        history = new_history;
                    }
                } else {
                    rpmspec_failures = 0;
                    if let Some(log) = attempt_log {
                        log.complete();
                    }
                    fix_count += 1;
                    spec_content = fixed;
                    history = new_history;
                }
            }
            Err(e) => {
                log(&format!("[{pkg}]    agent error: {e}"));
                if let Some(log) = attempt_log {
                    log.fail(&e.to_string());
                }
            }
        }
    }

    missing_packages.sort_unstable();
    missing_packages.dedup();
    Ok(missing_packages)
}

// ── dependency graph / topo sort ──────────────────────────────────────────────

pub(crate) fn resolve_cached(
    pkg: &str,
    work_dir: &Path,
) -> color_eyre::Result<Vec<ap_ecosystem::pip::Node>> {
    let cache_dir = work_dir.join("cache").join("resolve");
    fs::create_dir_all(&cache_dir)?;
    let cache_file = cache_dir.join(format!("{}.json", normalize(pkg)));

    if cache_file.exists() {
        let data = fs::read_to_string(&cache_file)?;
        let nodes: Vec<ap_ecosystem::pip::Node> = serde_json::from_str(&data)?;
        return Ok(nodes);
    }

    let nodes = ap_ecosystem::pip::resolve(pkg, None)?;
    fs::write(&cache_file, serde_json::to_string(&nodes)?)?;
    Ok(nodes)
}

fn pypi_name_from_rpm(rpm: &str) -> Option<String> {
    let rest = rpm.strip_prefix("python")?;
    let after_digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    after_digits.strip_prefix('-').map(str::to_string)
}

pub(crate) fn normalize(name: &str) -> String {
    name.to_lowercase().replace('-', "_")
}

/// Extract PyPI package names from `ModuleNotFoundError: No module named 'X'`
/// lines in a build log.
///
/// Only the top-level name is extracted (`foo` from `foo.bar`).
/// Underscores are normalised to hyphens to match PyPI conventions.
fn extract_missing_modules(log: &str) -> Vec<String> {
    const NEEDLE: &str = "ModuleNotFoundError: No module named '";
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in log.lines() {
        if let Some(after) = line.find(NEEDLE).map(|i| &line[i + NEEDLE.len()..]) {
            if let Some(end) = after.find('\'') {
                let full = &after[..end];
                let top = full.split('.').next().unwrap_or(full);
                let normalised = top.replace('_', "-").to_lowercase();
                if !normalised.is_empty() && seen.insert(normalised.clone()) {
                    found.push(normalised);
                }
            }
        }
    }
    found
}

/// True when every added or removed line in the diff is a comment (`# …`) or blank.
/// Used to detect agent responses that only touch `# ap: fix N:` annotations
/// without making any substantive spec change.
fn is_comment_only_diff(before: &str, after: &str) -> bool {
    let mut has_change = false;
    for d in diff::lines(before, after) {
        match d {
            diff::Result::Left(l) | diff::Result::Right(l) => {
                has_change = true;
                let t = l.trim();
                if !t.is_empty() && !t.starts_with('#') {
                    return false;
                }
            }
            diff::Result::Both(_, _) => {}
        }
    }
    has_change
}

/// Return true when the spec at `path` records a `Version:` that differs from `version`.
fn spec_version_differs(path: &Path, version: &str) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return true;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Version:") {
            return rest.trim() != version;
        }
    }
    true
}

pub(crate) fn dep_name(req: &str) -> Option<&str> {
    if req.contains("extra ==") || req.contains("extra==") {
        return None;
    }
    let end = req
        .find(['[', ';', '=', '<', '>', '!', '~', ' ', '\t'])
        .unwrap_or(req.len());
    let name = req[..end].trim();
    if name.is_empty() { None } else { Some(name) }
}

pub(crate) fn build_dep_graph(nodes: &[Node]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let n = nodes.len();
    let name_to_idx: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (normalize(&node.name), i))
        .collect();

    let mut reverse: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, node) in nodes.iter().enumerate() {
        let mut seen = HashSet::new();
        for req in &node.requires_dist {
            if let Some(name) = dep_name(req) {
                let key = normalize(name);
                if seen.contains(&key) {
                    continue;
                }
                if let Some(&j) = name_to_idx.get(&key)
                    && i != j
                {
                    in_degree[i] += 1;
                    reverse[j].push(i);
                    seen.insert(key);
                }
            }
        }
    }

    (in_degree, reverse)
}

/// Return indices of `nodes` in topological order (dependencies before dependants).
///
/// Uses Kahn's algorithm. Nodes that form cycles are appended last in their
/// original order. This is the single canonical ordering used by both the
/// build scheduler and the TUI display.
pub(crate) fn topo_sort(nodes: &[Node]) -> Vec<usize> {
    let n = nodes.len();
    let (mut in_degree, reverse) = build_dep_graph(nodes);
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        order.push(idx);
        for &j in &reverse[idx] {
            in_degree[j] -= 1;
            if in_degree[j] == 0 {
                queue.push_back(j);
            }
        }
    }
    if order.len() < n {
        let in_order: HashSet<usize> = order.iter().copied().collect();
        for i in 0..n {
            if !in_order.contains(&i) {
                order.push(i);
            }
        }
    }
    order
}
