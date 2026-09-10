//! Command-line surface.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "auto-package", about = "Automated RPM packaging for openSUSE")]
struct Cli {
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

    // 1. Resolve the full dependency tree.
    eprintln!("[1/4] Resolving dependencies for {name}…");
    let nodes = ap_ecosystem::pip::resolve(name, None)?;
    eprintln!("      {} package(s) in closure", nodes.len());

    // 2. For each node: fetch metadata, download sdist, generate spec, build.
    //    pip returns nodes in resolution order (leaves tend to come first).
    for node in &nodes {
        eprintln!("\n[pkg] {} {}", node.name, node.version);

        // Fetch the PyPI JSON for this exact version to get the sdist URL.
        let pypi_url = format!("https://pypi.org/pypi/{}/{}/json", node.name, node.version);
        let json = ureq::get(&pypi_url).call()?.into_body().read_to_string()?;
        let release = ap_ecosystem::pypi::parse_release(&json)?;

        if release.yanked {
            eprintln!("      SKIP: yanked");
            continue;
        }

        // Package directory inside work_dir.
        let pkg_dir = work_dir.join(node.name.to_lowercase());
        fs::create_dir_all(&pkg_dir)?;

        // Download the sdist.
        eprintln!("      downloading sdist…");
        let tarball = release.sdist.fetch(&pkg_dir)?;

        // Inspect for build-system requirements.
        let build_system = ap_ecosystem::sdist::build_system(&tarball)?;

        // Generate the spec.
        eprintln!("      generating spec…");
        let spec_content = ap_build::spec::generate(&release, &build_system)?;
        let spec_path = pkg_dir.join(format!("python-{}.spec", node.name.to_lowercase()));
        fs::write(&spec_path, &spec_content)?;

        // Build.
        eprintln!("      running osc build…");
        let mut attempts = 0;
        const MAX_BUILD_ATTEMPTS: usize = 6;

        loop {
            match ap_build::osc::build(&pkg_dir, &spec_path, &rpms_dir, distro, arch) {
                Ok(()) => {
                    eprintln!("      BUILD OK");
                    break;
                }
                Err(ap_build::osc::Error::BuildFailed { log, .. }) => {
                    attempts += 1;
                    if attempts >= MAX_BUILD_ATTEMPTS {
                        eprintln!("      FAILED after {attempts} attempts");
                        break;
                    }
                    eprintln!("      build failed, asking agent to fix (attempt {attempts})…");
                    match ap_build::agent::fix(&spec_path, &log, &node.name) {
                        Ok((fixed, usage)) => {
                            total_usage.add(&usage);
                            fs::write(&spec_path, &fixed)?;
                        }
                        Err(e) => {
                            eprintln!("      agent error: {e}");
                            break;
                        }
                    }
                }
                Err(e) => {
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
