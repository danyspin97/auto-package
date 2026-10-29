//! The per-project lock a run takes, and the check that a project directory
//! is not a symlink.

use super::*;

/// Name of the per-project lock file, kept inside the project directory.
const PROJECT_LOCK_NAME: &str = ".auto-package.lock";

pub(super) fn reject_symlink_project_dir(dir: &Path) -> color_eyre::Result<()> {
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
pub(super) struct ProjectLock {
    _file: std::fs::File,
}

impl ProjectLock {
    pub(super) fn acquire(project_dir: &Path) -> color_eyre::Result<Self> {
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
