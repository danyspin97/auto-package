//! Per-package workspace: a persistent directory under `<work-dir>/state/<rpm-pkg>/`
//! that holds all build artefacts outside the osc checkout.
//!
//! Layout (from REDESIGN.md §2.2):
//! ```text
//! <work-dir>/state/<rpm-pkg>/
//!   spec              → symlink to the spec inside the osc checkout (writable)
//!   spec.generated      pristine generated spec, for diff and rollback
//!   build.log           full build log from the last osc build
//!   rpmspec.txt         stderr of the last rpmspec --parse that failed
//!   sdist/              extracted tarball (persists across runs)
//!   notes.md            append-only attempt journal
//!   state.json          PkgState (added in P7)
//! ```
//!
//! The osc working copy holds **only** the spec and the tarball, so `osc status`
//! stays clean.

use std::fs;
use std::path::{Path, PathBuf};

pub struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    /// Open (creating if absent) the workspace for `rpm_pkg` under `state_root`.
    pub fn open(state_root: &Path, rpm_pkg: &str) -> std::io::Result<Self> {
        let dir = state_root.join(rpm_pkg);
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Root directory of the workspace.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path where the full `osc build` log is written and read.
    pub fn build_log(&self) -> PathBuf {
        self.dir.join("build.log")
    }

    /// Path where `rpmspec --parse` stderr is written on failure.
    pub fn rpmspec_log(&self) -> PathBuf {
        self.dir.join("rpmspec.txt")
    }

    /// Directory where the sdist tarball is extracted.
    ///
    /// The caller is responsible for populating this directory; existence of any
    /// entry inside it means extraction has already happened.
    pub fn sdist_dir(&self) -> PathBuf {
        self.dir.join("sdist")
    }

    /// Create or refresh the `spec` symlink pointing at `spec_path`.
    ///
    /// `spec_path` should be an absolute path so the symlink remains valid
    /// regardless of the working directory.
    #[cfg(unix)]
    pub fn link_spec(&self, spec_path: &Path) -> std::io::Result<()> {
        let link = self.dir.join("spec");
        // Remove stale symlink or file if present.
        if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(spec_path, &link)
    }

    /// True when the sdist directory exists and is non-empty (already extracted).
    pub fn sdist_extracted(&self) -> bool {
        let dir = self.sdist_dir();
        dir.exists()
            && fs::read_dir(&dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
    }

    /// Path of the `buildroot/` symlink inside the workspace.
    pub fn buildroot_link(&self) -> PathBuf {
        self.dir.join("buildroot")
    }

    /// Create or refresh the `buildroot/` symlink pointing at `target`.
    ///
    /// If `target` is `None`, the symlink is removed (if it exists).
    #[cfg(unix)]
    pub fn link_buildroot(&self, target: Option<&Path>) -> std::io::Result<()> {
        let link = self.buildroot_link();
        // Remove stale entry if present.
        if link.symlink_metadata().is_ok() {
            // Symlink to a directory — remove the symlink itself, not its contents.
            fs::remove_file(&link)?;
        }
        if let Some(t) = target {
            std::os::unix::fs::symlink(t, &link)?;
        }
        Ok(())
    }
}
