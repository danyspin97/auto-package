//! Command-line surface.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use tracing::{debug, info, warn};

fn print_diff(before: &str, after: &str) {
    eprintln!("      --- agent changes ---");
    for diff in diff::lines(before, after) {
        match diff {
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

        /// OBS distribution target.
        #[arg(long, default_value = "openSUSE_Tumbleweed")]
        distro: String,

        /// Architecture.
        #[arg(long, default_value = "x86_64")]
        arch: String,
    },
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
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
        } => {
            cmd_build(&name, &work_dir, &distro, &arch)?;
        }
    }

    Ok(())
}

fn cmd_build(name: &str, work_dir: &Path, distro: &str, arch: &str) -> color_eyre::Result<()> {
    let rpms_dir = work_dir.join("rpms");
    fs::create_dir_all(&rpms_dir)?;

    let mut total_usage = ap_build::agent::Usage::default();

    info!("resolving dependencies for {name}");
    let nodes = ap_ecosystem::pip::resolve(name, None)?;
    info!("{} package(s) in closure", nodes.len());
    for n in &nodes {
        debug!("  {} {}", n.name, n.version);
    }

    for node in &nodes {
        eprintln!("\n[pkg] {} {}", node.name, node.version);

        info!("fetching PyPI metadata");
        let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
        let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
        let release = ap_ecosystem::pypi::parse_release(&json)?;
        debug!("license: {:?}", release.license);

        if release.yanked {
            warn!("skipping {} {}: yanked", node.name, node.version);
            eprintln!("      SKIP: yanked");
            continue;
        }

        let pkg_dir = work_dir.join(node.name.to_lowercase());
        fs::create_dir_all(&pkg_dir)?;

        info!("downloading sdist: {}", release.sdist.filename);
        let tarball = release.sdist.fetch(&pkg_dir)?;
        debug!("sdist at {}", tarball.display());

        info!("inspecting build system");
        let build_system = ap_ecosystem::sdist::build_system(&tarball)?;
        info!(
            "build-backend: {}, requires: {:?}",
            build_system.backend.as_deref().unwrap_or("(none)"),
            build_system.requires,
        );

        info!("generating spec via py2pack");
        let mut spec_content = ap_build::spec::generate(&release, &build_system)?;
        let spec_path = pkg_dir.join(format!("python-{}.spec", node.name.to_lowercase()));
        fs::write(&spec_path, &spec_content)?;
        eprintln!("      files in {}", pkg_dir.display());
        debug!("spec written to {}", spec_path.display());

        let mut attempts = 0;
        const MAX_BUILD_ATTEMPTS: usize = 6;

        loop {
            info!("running osc build (attempt {})", attempts + 1);
            match ap_build::osc::build(&pkg_dir, &spec_path, &rpms_dir, distro, arch) {
                Ok(()) => {
                    info!("build succeeded");
                    eprintln!("      BUILD OK");
                    break;
                }
                Err(ap_build::osc::Error::BuildFailed { log, .. }) => {
                    attempts += 1;
                    warn!(
                        "build failed (attempt {attempts}), log at {}",
                        log.display()
                    );

                    // Show the extracted error lines so the user can see
                    // what went wrong without opening the full log.
                    let log_raw = fs::read_to_string(&log).unwrap_or_default();
                    let mapper = ap_build::log::PathMapper::with_sdist_root(&node.name);
                    let extracted = ap_build::log::extract(&log_raw, &mapper);
                    if let Some(section) = &extracted.failed_section {
                        eprintln!("      failed in: {section}");
                    }
                    for line in &extracted.error_lines {
                        eprintln!("      error: {line}");
                    }
                    if !extracted.excerpt.is_empty() {
                        debug!("excerpt:\n{}", extracted.excerpt);
                    }

                    if attempts >= MAX_BUILD_ATTEMPTS {
                        eprintln!("      FAILED after {attempts} attempts");
                        break;
                    }
                    eprintln!("      asking agent to fix (attempt {attempts})…");
                    info!("invoking agent");
                    match ap_build::agent::fix(&spec_path, &log, &node.name) {
                        Ok((fixed, usage)) => {
                            info!(
                                "agent fix: {} in / {} out",
                                usage.input_tokens, usage.output_tokens
                            );
                            total_usage.add(&usage);

                            // Show what the agent changed.
                            print_diff(&spec_content, &fixed);

                            fs::write(&spec_path, &fixed)?;
                            spec_content = fixed;
                        }
                        Err(e) => {
                            warn!("agent error: {e}");
                            eprintln!("      agent error: {e}");
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("osc error: {e}");
                    eprintln!("      osc error: {e}");
                    break;
                }
            }
        }
    }

    if total_usage.input_tokens > 0 {
        eprintln!(
            "\n[cost] {} in / {} out / {} cache-write / {} cache-read → ${:.4}",
            total_usage.input_tokens,
            total_usage.output_tokens,
            total_usage.cache_creation_tokens,
            total_usage.cache_read_tokens,
            total_usage.cost_cents() / 100.0,
        );
    }

    Ok(())
}
