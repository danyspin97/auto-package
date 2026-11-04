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

        /// List the pending baselines that would be dropped, and drop nothing.
        #[arg(long)]
        dry_run: bool,

        /// Drop them without asking. Without a terminal this is required.
        #[arg(short, long)]
        yes: bool,
    },
}

/// The pending OBS baselines of `name` that the build of `project` (in
/// `work_dir`) and the update and maintenance runs of the same work directory
/// kept.
fn pending_baselines(work_dir: &Path, project: &str, arch: &str, name: &str) -> Vec<PathBuf> {
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
    candidates.retain(|dir| ap_build::osc::is_pending(dir));
    candidates
}

/// The question before pending baselines are dropped: they are listed on
/// stderr and the answer is read from the terminal. Without a terminal the
/// question cannot be asked, so the command is refused.
fn confirm_discard(name: &str, pending: &[PathBuf]) -> color_eyre::Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    for dir in pending {
        eprintln!(
            "{name}: would discard the pending baseline in {}",
            dir.display()
        );
    }
    if !std::io::stdin().is_terminal() {
        return Err(color_eyre::eyre::eyre!(
            "{name}: discarding asks for confirmation and there is no terminal: pass --yes, or --dry-run to only list them"
        ));
    }
    eprint!("Discard them? OBS is taken as it is. [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Runs one `baseline` subcommand and prints what it did.
pub(super) fn baseline_command(command: BaselineCommand) -> color_eyre::Result<()> {
    match command {
        BaselineCommand::Discard {
            name,
            project,
            arch,
            work_dir,
            dry_run,
            yes,
        } => {
            let pending = pending_baselines(&work_dir, &project, &arch, &name);
            if pending.is_empty() {
                println!("{name}: no pending OBS baseline in {}", work_dir.display());
                return Ok(());
            }
            if dry_run {
                for dir in &pending {
                    println!(
                        "{name}: would discard the pending baseline in {}",
                        dir.display()
                    );
                }
                return Ok(());
            }
            if !yes && !confirm_discard(&name, &pending)? {
                println!("{name}: kept the pending baseline");
                return Ok(());
            }
            for dir in pending {
                if ap_build::osc::discard_pending(&dir)? {
                    println!(
                        "{name}: discarded the pending baseline in {}; OBS is taken as it is",
                        dir.display()
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        command: BaselineCommand,
    }

    #[test]
    fn only_a_baseline_with_a_pending_marker_is_listed() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path();
        let dir = w.join("Base_System-x86_64/state/foo/obs-baseline");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(pending_baselines(w, "Base:System", "x86_64", "foo").is_empty());
        // The marker of a pending baseline (`osc::UNRESTORED`).
        std::fs::write(dir.join("unrestored"), "").unwrap();
        assert_eq!(
            pending_baselines(w, "Base:System", "x86_64", "foo"),
            vec![dir]
        );
    }

    #[test]
    fn discard_takes_dry_run_and_yes() {
        let Wrapper { command } =
            Wrapper::try_parse_from(["x", "discard", "foo", "--project", "P", "-y"]).unwrap();
        let BaselineCommand::Discard { yes, dry_run, .. } = command;
        assert!(yes && !dry_run);
    }
}
