//! The baseline subcommand and the discarding of pending baselines.

use super::*;

#[derive(Subcommand)]
pub(super) enum BaselineCommand {
    /// Drop the pending baseline of a package without putting it back, so OBS
    /// is taken as it is. Check OBS first: a failed attempt stays there.
    Discard {
        /// RPM package name, as the build names it.
        name: String,

        /// OBS project the package was built for.
        #[arg(long)]
        project: String,

        #[arg(long, default_value = "x86_64")]
        arch: String,

        /// The work directory the build or update used.
        #[arg(long, default_value = "ap-work")]
        work_dir: PathBuf,
    },
}

/// Discard the pending OBS baselines of `name` that the build of `project`
/// (in `work_dir`) and the update and maintenance runs of the same work
/// directory kept. Returns the directories removed.
pub(super) fn discard_pending_baselines(
    work_dir: &Path,
    project: &str,
    arch: &str,
    name: &str,
) -> std::io::Result<Vec<PathBuf>> {
    let project_dir = work_dir.join(format!(
        "{}-{}",
        ap_core::run::sanitize_name(project),
        ap_core::run::sanitize_name(arch),
    ));
    let mut candidates = vec![
        project_dir
            .join("state")
            .join(ap_core::run::sanitize_name(name))
            .join("obs-baseline"),
    ];
    candidates.extend(ap_build::osc::baselines_of(
        &project_dir.join(ap_build::osc::BASELINE_ROOT),
        name,
    ));
    candidates.extend(ap_build::osc::baselines_of(
        &work_dir
            .join("maintenance")
            .join(ap_build::osc::BASELINE_ROOT),
        name,
    ));
    let mut removed = Vec::new();
    for dir in candidates {
        if ap_build::osc::discard_pending(&dir)? {
            removed.push(dir);
        }
    }
    Ok(removed)
}

/// Runs one `baseline` subcommand and prints what it did.
pub(super) fn baseline_command(command: BaselineCommand) -> color_eyre::Result<()> {
    match command {
        BaselineCommand::Discard {
            name,
            project,
            arch,
            work_dir,
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
    }
    Ok(())
}
