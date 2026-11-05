//! The baseline subcommand and the discarding of pending baselines.

use super::*;
use std::fs;

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

        /// The arch the build or update used. Without it every arch of the
        /// project in the work directory is searched.
        #[arg(long)]
        arch: Option<String>,

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

/// The project directories of `project` (the branch project the build or the
/// update wrote to) in the work directory `work_dir`: `<project>-<arch>`, for
/// the arch given, or for each arch there is one.
fn project_dirs(work_dir: &Path, project: &str, arch: Option<&str>) -> Vec<PathBuf> {
    let branch = ap_core::run::sanitize_name(project);
    match arch {
        Some(arch) => {
            vec![work_dir.join(format!("{branch}-{}", ap_core::run::sanitize_name(arch)))]
        }
        None => {
            let prefix = format!("{branch}-");
            fs::read_dir(work_dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .and_then(|n| n.strip_prefix(&prefix))
                            .is_some_and(|arch| !arch.is_empty() && !arch.contains('-'))
                })
                .collect()
        }
    }
}

/// Whether `name` is a plain OBS package name: it is joined into paths, so it
/// must not climb out of its directory, name another one, or be empty.
fn plain_package_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+~".contains(&b))
}

/// Whether the baseline in `dir` was saved from a checkout of `project`: the
/// checkout's `.osc/_project` names it, or, when the checkout is gone, its path
/// has `project` as a component. Two projects can share a directory name once
/// sanitized (`home:B` and `home_B`), so the directory alone does not say.
fn baseline_of_project(dir: &Path, project: &str) -> bool {
    let Ok(checkout) = fs::read_to_string(dir.join("checkout")) else {
        return false;
    };
    let checkout = PathBuf::from(checkout);
    match fs::read_to_string(checkout.join(".osc/_project")) {
        Ok(recorded) => recorded.trim() == project,
        Err(_) => checkout
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new(project)),
    }
}

/// The pending OBS baselines of `name` for the branch `project` in the
/// workspace `work_dir`: the build's own state, the baselines the update and
/// `tidy` keep in the project directories, and the maintenance ones. Only the
/// baselines of `project` are named: other branches' are not.
fn pending_baselines(
    work_dir: &Path,
    project: &str,
    arch: Option<&str>,
    name: &str,
) -> Vec<PathBuf> {
    if !plain_package_name(name) {
        return Vec::new();
    }
    let branch = ap_core::run::sanitize_name(project);
    let mut candidates = Vec::new();
    for dir in project_dirs(work_dir, project, arch) {
        candidates.push(
            dir.join("state")
                .join(ap_core::run::sanitize_name(name))
                .join("obs-baseline"),
        );
        candidates.push(
            dir.join(ap_build::osc::BASELINE_ROOT)
                .join(&branch)
                .join(name),
        );
    }
    candidates.push(
        work_dir
            .join("maintenance")
            .join(ap_build::osc::BASELINE_ROOT)
            .join(&branch)
            .join(name),
    );
    candidates.retain(|dir| ap_build::osc::is_pending(dir) && baseline_of_project(dir, project));
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
            let pending = pending_baselines(&work_dir, &project, arch.as_deref(), &name);
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

    #[test]
    fn a_name_that_is_not_a_plain_package_name_names_nothing() {
        let ws = tempfile::tempdir().unwrap();
        let mine = ws.path().join("home_B-x86_64/.obs-baseline/home_B/foo");
        pending_at(&mine, "home:B");
        let other = ws.path().join("home_B-x86_64/.obs-baseline/home_B2/foo");
        pending_at(&other, "home:B2");
        for name in ["../home_B2/foo", "foo/bar", "", "..", "a\0b"] {
            assert!(
                pending_baselines(ws.path(), "home:B", None, name).is_empty(),
                "{name:?}"
            );
        }
        assert!(plain_package_name("python-foo_bar.2+x"));
    }

    #[test]
    fn a_baseline_of_a_project_sharing_the_directory_name_is_not_named() {
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("home_B-x86_64/.obs-baseline/home_B/foo");
        pending_at(&dir, "home:B");
        let co = ws.path().join("checkouts/home_B/foo");
        fs::create_dir_all(co.join(".osc")).unwrap();
        fs::write(co.join(".osc/_project"), "home_B\n").unwrap();
        fs::write(dir.join("checkout"), co.to_string_lossy().as_bytes()).unwrap();
        assert!(pending_baselines(ws.path(), "home:B", None, "foo").is_empty());
        assert_eq!(
            pending_baselines(ws.path(), "home_B", None, "foo"),
            vec![dir]
        );
    }

    /// A baseline directory with one attempted file, so it is pending.
    fn pending_at(dir: &Path, project: &str) {
        fs::create_dir_all(dir).unwrap();
        ap_build::osc::note_attempt(dir, &["foo.spec".to_string()]).unwrap();
        // Saved from a checkout of `project` (not on disk any more).
        fs::write(dir.join("checkout"), format!("/x/{project}/foo")).unwrap();
    }

    #[test]
    fn a_tidy_baseline_is_found_from_the_workspace_root() {
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("home_B-x86_64/.obs-baseline/home_B/foo");
        pending_at(&dir, "home:B");
        assert_eq!(
            pending_baselines(ws.path(), "home:B", None, "foo"),
            vec![dir.clone()]
        );
        assert_eq!(
            pending_baselines(ws.path(), "home:B", Some("x86_64"), "foo"),
            vec![dir]
        );
    }

    #[test]
    fn another_branches_baseline_of_the_same_package_survives() {
        let ws = tempfile::tempdir().unwrap();
        let mine = ws.path().join("home_B-x86_64/.obs-baseline/home_B/foo");
        let other = ws.path().join("home_B-x86_64/.obs-baseline/home_B2/foo");
        pending_at(&mine, "home:B");
        pending_at(&other, "home:B2");
        assert_eq!(
            pending_baselines(ws.path(), "home:B", None, "foo"),
            vec![mine]
        );
    }

    #[test]
    fn a_pipeline_state_is_found_for_its_arch_only_when_asked_for_it() {
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("home_B-x86_64/state/foo/obs-baseline");
        pending_at(&dir, "home:B");
        assert_eq!(
            pending_baselines(ws.path(), "home:B", Some("x86_64"), "foo"),
            vec![dir.clone()]
        );
        assert!(pending_baselines(ws.path(), "home:B", Some("aarch64"), "foo").is_empty());
        assert_eq!(
            pending_baselines(ws.path(), "home:B", None, "foo"),
            vec![dir]
        );
    }

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
        assert!(pending_baselines(w, "Base:System", Some("x86_64"), "foo").is_empty());
        // The marker of a pending baseline (`osc::UNRESTORED`).
        std::fs::write(dir.join("unrestored"), "").unwrap();
        // Saved from a checkout of this project.
        std::fs::write(dir.join("checkout"), "/x/Base:System/foo").unwrap();
        assert_eq!(
            pending_baselines(w, "Base:System", Some("x86_64"), "foo"),
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
