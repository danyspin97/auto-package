//! Command-line surface.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ap_ecosystem::pip::Node;
use clap::{Parser, Subcommand};
use tracing::{debug, warn};

/// Persist one fix attempt to disk for later review and prompt improvement.
///
/// Layout: `<fixes_dir>/<pkg>/attempt_<N>/`
///   error.txt       — the error excerpt the agent was shown
///   prompt.txt      — the full prompt including history
///   reasoning.txt   — one-line agent reasoning
///   diff.txt        — +/- lines changed
///   spec_before.spec
///   spec_after.spec
fn save_attempt(
    fixes_dir: &Path,
    pkg: &str,
    attempt: usize,
    turn: &ap_build::agent::Turn,
    spec_before: &str,
    spec_after: &str,
) {
    let dir = fixes_dir
        .join(pkg.to_lowercase())
        .join(format!("attempt_{attempt}"));
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let mut diff = String::new();
    for d in diff::lines(spec_before, spec_after) {
        match d {
            diff::Result::Left(l) => {
                diff.push('-');
                diff.push(' ');
                diff.push_str(l);
                diff.push('\n');
            }
            diff::Result::Right(r) => {
                diff.push('+');
                diff.push(' ');
                diff.push_str(r);
                diff.push('\n');
            }
            diff::Result::Both(_, _) => {}
        }
    }

    let _ = fs::write(dir.join("error.txt"), &turn.error_excerpt);
    let _ = fs::write(dir.join("prompt.txt"), &turn.prompt);
    let _ = fs::write(dir.join("reasoning.txt"), &turn.reasoning);
    let _ = fs::write(dir.join("diff.txt"), diff);
    let _ = fs::write(dir.join("spec_before.spec"), spec_before);
    let _ = fs::write(dir.join("spec_after.spec"), spec_after);
}

/// Print a step line: `[pkg]  step text`
fn step(pkg: &str, text: &str) {
    eprintln!("[{pkg}]  {text}");
}

/// Print a result appended to the previous step (used after a completed action).
fn result(pkg: &str, text: &str) {
    eprintln!("[{pkg}]    → {text}");
}

/// Print a diff of what the agent changed.
fn print_diff(pkg: &str, before: &str, after: &str) {
    for d in diff::lines(before, after) {
        match d {
            diff::Result::Left(l) => eprintln!("[{pkg}]    - {l}"),
            diff::Result::Right(r) => eprintln!("[{pkg}]    + {r}"),
            diff::Result::Both(_, _) => {}
        }
    }
}

/// Print the error excerpt with a simple border.
fn print_excerpt(pkg: &str, excerpt: &str) {
    let bar = "─".repeat(40);
    eprintln!("[{pkg}]    {bar}");
    for line in excerpt.lines() {
        eprintln!("[{pkg}]    {line}");
    }
    eprintln!("[{pkg}]    {bar}");
}

#[derive(Parser)]
#[command(name = "auto-package", about = "Automated RPM packaging for openSUSE")]
struct Cli {
    /// Enable verbose logging (-v info, -vv debug).
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

        /// Working directory for specs, sources, and built RPMs.
        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,

        /// Directory to save each agent fix attempt for later review.
        /// Defaults to <work-dir>/fixes/.
        #[arg(long)]
        fixes_dir: Option<PathBuf>,

        /// OBS distribution target.
        #[arg(long, default_value = "openSUSE_Tumbleweed")]
        distro: String,

        /// Architecture.
        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// OBS project to build packages under (e.g. home:you:auto-package).
        #[arg(long)]
        project: String,

        /// Send the full build log to the agent at Level::FullLog.
        /// WARNING: can exceed 34k tokens — use only when lower rungs have failed.
        #[arg(long, default_value_t = false)]
        allow_full_log: bool,
    },
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
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
        } => {
            let fixes_dir = fixes_dir.unwrap_or_else(|| work_dir.join("fixes"));
            cmd_build(
                &name,
                &work_dir,
                &fixes_dir,
                &project,
                &distro,
                &arch,
                allow_full_log,
            )?;
        }
    }

    Ok(())
}

fn cmd_build(
    name: &str,
    work_dir: &Path,
    fixes_dir: &Path,
    project: &str,
    distro: &str,
    arch: &str,
    allow_full_log: bool,
) -> color_eyre::Result<()> {
    let rpms_dir = work_dir.join("rpms");
    fs::create_dir_all(&rpms_dir)?;

    eprintln!("resolving {name}...");
    let nodes = ap_ecosystem::pip::resolve(name, None)?;
    eprintln!("  {} package(s) in closure", nodes.len());
    for n in &nodes {
        debug!("  {} {}", n.name, n.version);
    }

    let levels = topo_levels(&nodes);

    let total_usage = Mutex::new(ap_build::agent::Usage::default());

    for (level_idx, level) in levels.iter().enumerate() {
        let names: Vec<&str> = level.iter().map(|&j| nodes[j].name.as_str()).collect();
        eprintln!("\n[level {level_idx}]  {}", names.join(" · "));

        std::thread::scope(|s| {
            let handles: Vec<_> = level
                .iter()
                .map(|&idx| {
                    let node = &nodes[idx];
                    let ctx = BuildCtx {
                        work_dir,
                        rpms_dir: &rpms_dir,
                        fixes_dir,
                        project,
                        distro,
                        arch,
                        allow_full_log,
                    };
                    let total_usage = &total_usage;

                    s.spawn(move || match build_one(node, &ctx) {
                        Ok(usage) => {
                            total_usage.lock().unwrap().add(&usage);
                        }
                        Err(e) => {
                            warn!("{} failed: {e:#}", node.name);
                            eprintln!("[{}]  ✗ {e:#}", node.name);
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().expect("thread panicked");
            }
        });
    }

    let usage = total_usage.into_inner().unwrap();
    let cost = usage.cost_usd();
    if cost > 0.0 {
        eprintln!("\ncost  ${cost:.4}");
    }

    Ok(())
}

/// Path of the success marker for a package.
fn marker_path(pkg_dir: &Path) -> PathBuf {
    pkg_dir.join("BUILD_OK")
}

/// True if this package version already built successfully in a previous run.
fn already_built(pkg_dir: &Path, version: &str) -> bool {
    fs::read_to_string(marker_path(pkg_dir))
        .map(|v| v.trim() == version)
        .unwrap_or(false)
}

struct BuildCtx<'a> {
    work_dir: &'a Path,
    rpms_dir: &'a Path,
    fixes_dir: &'a Path,
    project: &'a str,
    distro: &'a str,
    arch: &'a str,
    allow_full_log: bool,
}

fn build_one(node: &Node, ctx: &BuildCtx<'_>) -> color_eyre::Result<ap_build::agent::Usage> {
    let work_dir = ctx.work_dir;
    let rpms_dir = ctx.rpms_dir;
    let fixes_dir = ctx.fixes_dir;
    let project = ctx.project;
    let distro = ctx.distro;
    let arch = ctx.arch;
    let allow_full_log = ctx.allow_full_log;
    let pkg = &node.name;
    let rpm_pkg = ap_build::osc::rpm_name(&node.name);

    eprintln!("\n[{pkg} {}]", node.version);

    // Create the OBS package if needed and check it out.
    // The checkout dir is the single working directory for everything.
    step(pkg, &format!("creating {project}/{rpm_pkg}"));
    ap_build::osc::ensure_package(project, &rpm_pkg, work_dir)?;

    step(pkg, "checking out");
    let pkg_dir = ap_build::osc::checkout(project, &rpm_pkg, work_dir)?;
    result(pkg, pkg_dir.to_str().unwrap_or("?"));

    if already_built(&pkg_dir, &node.version) {
        eprintln!("[{pkg}]  already built {}", node.version);
        return Ok(ap_build::agent::Usage::default());
    }

    let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
    let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
    let release = ap_ecosystem::pypi::parse_release(&json)?;
    debug!("[{pkg}] license: {:?}", release.license);

    if release.yanked {
        eprintln!("[{pkg}]  ✗ yanked — skipping");
        return Ok(ap_build::agent::Usage::default());
    }

    step(pkg, &format!("downloading {}", release.sdist.filename));
    let tarball = release.sdist.fetch(&pkg_dir)?;

    let build_system = ap_ecosystem::sdist::build_system(&tarball)?;
    debug!("[{pkg}] build-backend: {:?}", build_system.backend);

    step(pkg, "generating spec");
    let mut spec_content = ap_build::spec::generate(&release, &build_system)?;
    let spec_path = pkg_dir.join(format!("python-{}.spec", node.name.to_lowercase()));

    spec_content = ap_build::spec::normalize_python_module_versions(&spec_content);

    let has_c = ap_ecosystem::sdist::has_c_extensions(&tarball);
    if has_c {
        spec_content = ap_build::spec::fix_c_extension_arch(&spec_content);
    }
    spec_content = ap_build::spec::fix_files_case(&spec_content, &release.name);
    if let Some(fixed) = ap_build::spec::fix_autosetup_dir(&spec_content, &tarball) {
        spec_content = fixed;
    }

    fs::write(&spec_path, &spec_content)?;
    result(pkg, spec_path.to_str().unwrap_or("?"));

    let mut total_usage = ap_build::agent::Usage::default();
    let mut history: Vec<ap_build::agent::Turn> = Vec::new();
    let mut attempts = 0;
    const MAX_BUILD_ATTEMPTS: usize = 6;

    step(pkg, "reviewing spec");
    match ap_build::agent::review(&spec_path, &node.name) {
        Ok(Some((fixed, usage))) => {
            total_usage.add(&usage);
            print_diff(pkg, &spec_content, &fixed);
            fs::write(&spec_path, &fixed)?;
            spec_content = fixed;
        }
        Ok(None) => {}
        Err(e) => warn!("[{pkg}] review error: {e}"),
    }

    let mut escalation = ap_build::context::Escalation::new(false);
    let _ = &escalation;

    loop {
        step(pkg, &format!("build {}/{MAX_BUILD_ATTEMPTS}", attempts + 1));

        match ap_build::osc::build(&pkg_dir, rpms_dir, distro, arch) {
            Ok(()) => {
                eprintln!("[{pkg}]  ✓ built");
                fs::write(marker_path(&pkg_dir), &node.version)?;
                break;
            }
            Err(ap_build::osc::Error::BuildFailed { log, .. }) => {
                attempts += 1;
                escalation = ap_build::context::Escalation::new(true);

                let log_raw = fs::read_to_string(&log).unwrap_or_default();
                let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                let extracted = ap_build::log::extract(&log_raw, &mapper);

                let section = extracted.failed_section.as_deref().unwrap_or("unknown");
                eprintln!("[{pkg}]  ✗ {section} failed");
                for line in &extracted.error_lines {
                    eprintln!("[{pkg}]    {line}");
                }

                if !extracted.excerpt.is_empty() {
                    print_excerpt(pkg, &extracted.excerpt);
                } else {
                    eprintln!("[{pkg}]    (extractor found nothing — last 20 log lines)");
                    let tail: Vec<&str> = log_raw.lines().rev().take(20).collect();
                    print_excerpt(pkg, &tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
                }

                if attempts >= MAX_BUILD_ATTEMPTS {
                    eprintln!("[{pkg}]  ✗ gave up after {attempts} attempts");
                    break;
                }

                step(pkg, &format!("agent ({:?})", escalation.level()));
                match ap_build::agent::fix(
                    &spec_path,
                    &log,
                    &node.name,
                    &history,
                    &escalation,
                    &tarball,
                    allow_full_log,
                ) {
                    Ok((fixed, usage, new_history, confidence)) => {
                        use ap_build::agent::Confidence;
                        use ap_build::context::EscalationReason;

                        total_usage.add(&usage);

                        if confidence == Confidence::Low {
                            let next = escalation.escalate(EscalationReason::RepeatedSignature);
                            let next_name = next
                                .as_ref()
                                .map(|l| format!("{l:?}"))
                                .unwrap_or_else(|_| "max".into());
                            eprintln!("[{pkg}]    low confidence → escalating to {next_name}");
                            history = new_history;
                        } else {
                            print_diff(pkg, &spec_content, &fixed);
                            save_attempt(
                                fixes_dir,
                                &node.name,
                                attempts,
                                &new_history[new_history.len() - 1],
                                &spec_content,
                                &fixed,
                            );
                            fs::write(&spec_path, &fixed)?;
                            spec_content = fixed;
                            history = new_history;
                        }
                    }
                    Err(e) => {
                        eprintln!("[{pkg}]    agent error: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[{pkg}]  ✗ osc error: {e}");
                break;
            }
        }
    }

    Ok(total_usage)
}

/// Normalize a package name for comparison: lowercase, hyphens → underscores.
fn normalize(name: &str) -> String {
    name.to_lowercase().replace('-', "_")
}

/// Extract the package name from a PEP 508 dependency string.
/// Returns `None` for extra-gated deps (`; extra == '...'`) since we are not
/// requesting extras.
fn dep_name(req: &str) -> Option<&str> {
    if req.contains("extra ==") || req.contains("extra==") {
        return None;
    }
    let end = req
        .find(['[', ';', '=', '<', '>', '!', '~', ' ', '\t'])
        .unwrap_or(req.len());
    let name = req[..end].trim();
    if name.is_empty() { None } else { Some(name) }
}

/// Group node indices into topological levels via Kahn's BFS.
///
/// Level 0 contains packages with no dependencies inside the closure (leaves).
/// Every package at level N depends only on packages at levels < N, so all
/// packages within a level can be built concurrently.
fn topo_levels(nodes: &[Node]) -> Vec<Vec<usize>> {
    let n = nodes.len();
    let name_to_idx: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (normalize(&node.name), i))
        .collect();

    // reverse[j] = indices of packages that directly depend on j.
    let mut reverse: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree: Vec<usize> = vec![0; n];

    for (i, node) in nodes.iter().enumerate() {
        let mut seen = std::collections::HashSet::new();
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

    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut levels = Vec::new();

    while !queue.is_empty() {
        levels.push(queue.clone());
        let mut next = Vec::new();
        for i in queue {
            for &j in &reverse[i] {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    next.push(j);
                }
            }
        }
        queue = next;
    }

    // Cycles shouldn't occur in pip-resolved deps, but don't deadlock if they do.
    let remaining: Vec<usize> = (0..n).filter(|&i| in_degree[i] > 0).collect();
    if !remaining.is_empty() {
        warn!(
            "{} node(s) in a dependency cycle — building sequentially",
            remaining.len()
        );
        levels.push(remaining);
    }

    levels
}
