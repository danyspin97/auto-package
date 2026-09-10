//! Command-line surface.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ap_ecosystem::pip::Node;
use clap::{Parser, Subcommand};
use tracing::{debug, info, warn};

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

fn status(pkg: &str, step: &str) {
    eprintln!("[{pkg}] {step}");
}

fn print_diff(before: &str, after: &str) {
    eprintln!("      --- agent changes ---");
    for d in diff::lines(before, after) {
        match d {
            diff::Result::Left(l) => eprintln!("      - {l}"),
            diff::Result::Right(r) => eprintln!("      + {r}"),
            diff::Result::Both(_, _) => {}
        }
    }
    eprintln!("      ---");
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
            allow_full_log,
        } => {
            let fixes_dir = fixes_dir.unwrap_or_else(|| work_dir.join("fixes"));
            cmd_build(&name, &work_dir, &fixes_dir, &distro, &arch, allow_full_log)?;
        }
    }

    Ok(())
}

fn cmd_build(
    name: &str,
    work_dir: &Path,
    fixes_dir: &Path,
    distro: &str,
    arch: &str,
    allow_full_log: bool,
) -> color_eyre::Result<()> {
    let rpms_dir = work_dir.join("rpms");
    fs::create_dir_all(&rpms_dir)?;

    info!("resolving dependencies for {name}");
    let nodes = ap_ecosystem::pip::resolve(name, None)?;
    info!("{} package(s) in closure", nodes.len());
    for n in &nodes {
        debug!("  {} {}", n.name, n.version);
    }

    let levels = topo_levels(&nodes);
    info!("{} topological level(s)", levels.len());
    for (i, level) in levels.iter().enumerate() {
        let names: Vec<_> = level.iter().map(|&j| nodes[j].name.as_str()).collect();
        debug!("  level {i}: {names:?}");
    }

    let total_usage = Mutex::new(ap_build::agent::Usage::default());

    for (level_idx, level) in levels.iter().enumerate() {
        eprintln!(
            "\n[level {level_idx}] {} package(s) in parallel",
            level.len()
        );

        std::thread::scope(|s| {
            let handles: Vec<_> = level
                .iter()
                .map(|&idx| {
                    let node = &nodes[idx];
                    let work_dir = work_dir.to_path_buf();
                    let rpms_dir = rpms_dir.clone();
                    let fixes_dir = fixes_dir.to_path_buf();
                    let distro = distro.to_string();
                    let arch = arch.to_string();
                    let total_usage = &total_usage;

                    s.spawn(move || {
                        match build_one(
                            node,
                            &work_dir,
                            &rpms_dir,
                            &fixes_dir,
                            &distro,
                            &arch,
                            allow_full_log,
                        ) {
                            Ok(usage) => {
                                total_usage.lock().unwrap().add(&usage);
                            }
                            Err(e) => {
                                warn!("{} failed: {e:#}", node.name);
                                eprintln!("\n[{}] FAILED: {e:#}", node.name);
                            }
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
    if usage.input_tokens > 0 {
        eprintln!(
            "\n[cost] {} in / {} out / {} cache-write / {} cache-read → ${:.4}",
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_tokens,
            usage.cache_read_tokens,
            usage.cost_usd(),
        );
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

fn build_one(
    node: &Node,
    work_dir: &Path,
    rpms_dir: &Path,
    fixes_dir: &Path,
    distro: &str,
    arch: &str,
    allow_full_log: bool,
) -> color_eyre::Result<ap_build::agent::Usage> {
    let pkg = &node.name;
    let pkg_dir = work_dir.join(node.name.to_lowercase());

    if already_built(&pkg_dir, &node.version) {
        info!("[{pkg}] already built, skipping");
        eprintln!("[{pkg}] SKIP: already built ({})", node.version);
        return Ok(ap_build::agent::Usage::default());
    }

    eprintln!("\n[{pkg}] {} {}", node.name, node.version);

    info!("[{pkg}] fetching PyPI metadata");
    let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
    let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
    let release = ap_ecosystem::pypi::parse_release(&json)?;
    debug!("[{pkg}] license: {:?}", release.license);

    if release.yanked {
        warn!("[{pkg}] skipping: yanked");
        eprintln!("[{pkg}] SKIP: yanked");
        return Ok(ap_build::agent::Usage::default());
    }

    fs::create_dir_all(&pkg_dir)?;

    status(pkg, &format!("downloading {}", release.sdist.filename));
    let tarball = release.sdist.fetch(&pkg_dir)?;
    debug!("[{pkg}] sdist at {}", tarball.display());

    let build_system = ap_ecosystem::sdist::build_system(&tarball)?;
    debug!(
        "[{pkg}] build-backend: {}, requires: {:?}",
        build_system.backend.as_deref().unwrap_or("(none)"),
        build_system.requires,
    );

    status(pkg, "generating spec");
    let mut spec_content = ap_build::spec::generate(&release, &build_system)?;
    let spec_path = pkg_dir.join(format!("python-{}.spec", node.name.to_lowercase()));
    fs::write(&spec_path, &spec_content)?;
    eprintln!("[{pkg}] spec at {}", spec_path.display());

    // Fix %autosetup -n if it doesn't match the actual tarball directory.
    // This is deterministic — no need for the agent.
    if let Some(fixed) = ap_build::spec::fix_autosetup_dir(&spec_content, &tarball) {
        info!("[{pkg}] fixed %autosetup -n to match sdist directory");
        eprintln!("[{pkg}] fixed %autosetup -n directory");
        fs::write(&spec_path, &fixed)?;
        spec_content = fixed;
    }

    let mut total_usage = ap_build::agent::Usage::default();
    let mut history: Vec<ap_build::agent::Turn> = Vec::new();
    let mut attempts = 0;
    const MAX_BUILD_ATTEMPTS: usize = 6;

    // Pre-build spec review: catch obvious issues before wasting a build slot.
    info!("[{pkg}] reviewing spec before build");
    status(pkg, "reviewing spec…");
    match ap_build::agent::review(&spec_path, &node.name) {
        Ok(Some((fixed, usage))) => {
            status(pkg, "pre-build fix applied");
            total_usage.add(&usage);
            print_diff(&spec_content, &fixed);
            fs::write(&spec_path, &fixed)?;
            spec_content = fixed;
        }
        Ok(None) => status(pkg, "review: spec OK"),
        Err(e) => warn!("[{pkg}] review error (continuing): {e}"),
    }

    let mut escalation = ap_build::context::Escalation::new(false);
    let _ = &escalation;

    loop {
        status(pkg, &format!("osc build (attempt {})…", attempts + 1));
        match ap_build::osc::build(&pkg_dir, &spec_path, rpms_dir, distro, arch) {
            Ok(()) => {
                info!("[{pkg}] build succeeded");
                eprintln!("[{pkg}] BUILD OK");
                fs::write(marker_path(&pkg_dir), &node.version)?;
                break;
            }
            Err(ap_build::osc::Error::BuildFailed { log, .. }) => {
                attempts += 1;
                // A build failure means BUILDROOT is now populated.
                escalation = ap_build::context::Escalation::new(true);
                warn!(
                    "[{pkg}] build failed (attempt {attempts}), log at {}",
                    log.display()
                );

                let log_raw = fs::read_to_string(&log).unwrap_or_default();
                let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                let extracted = ap_build::log::extract(&log_raw, &mapper);
                if let Some(section) = &extracted.failed_section {
                    eprintln!("[{pkg}] failed in: {section}");
                }
                for line in &extracted.error_lines {
                    eprintln!("[{pkg}] error: {line}");
                }
                if !extracted.excerpt.is_empty() {
                    debug!("[{pkg}] excerpt:\n{}", extracted.excerpt);
                }

                if attempts >= MAX_BUILD_ATTEMPTS {
                    eprintln!("[{pkg}] FAILED after {attempts} attempts");
                    break;
                }

                status(
                    pkg,
                    &format!(
                        "agent fix (level {:?}, attempt {attempts})…",
                        escalation.level()
                    ),
                );
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

                        info!(
                            "[{pkg}] agent: {} in / {} out, confidence {:?}",
                            usage.input_tokens, usage.output_tokens, confidence,
                        );
                        total_usage.add(&usage);

                        if confidence == Confidence::Low {
                            let next = escalation.escalate(EscalationReason::RepeatedSignature);
                            status(
                                pkg,
                                &format!(
                                    "low confidence — escalating to {:?}",
                                    next.as_ref()
                                        .map(|l| format!("{l:?}"))
                                        .unwrap_or_else(|_| "max".into())
                                ),
                            );
                            history = new_history;
                        } else {
                            print_diff(&spec_content, &fixed);
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
                        warn!("[{pkg}] agent error: {e}");
                        eprintln!("[{pkg}] agent error: {e}");
                    }
                }
            }
            Err(e) => {
                warn!("[{pkg}] osc error: {e}");
                eprintln!("[{pkg}] osc error: {e}");
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
