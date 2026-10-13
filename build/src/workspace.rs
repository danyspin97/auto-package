//! Per-package durable workspace: `<work-dir>/state/<rpm-pkg>/`.
//!
//! The durable workspace survives runs. Run-specific artifacts live in
//! `RunDir`/`PkgRunDir`/`StepDir` from ap-core.
//!
//! The osc working copy (`<work-dir>/<project>/<rpm-name>/`) holds ONLY
//! the spec and the tarball so `osc status` stays clean.

use std::fs;
use std::path::{Path, PathBuf};

use ap_core::run::sanitize_name;

pub struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    /// Open (creating if absent) the durable workspace for `rpm_pkg`.
    /// The package name is sanitized before joining the path.
    pub fn open(state_root: &Path, rpm_pkg: &str) -> std::io::Result<Self> {
        let dir = state_root.join(sanitize_name(rpm_pkg));
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Build log path (legacy — new code writes logs into StepDir).
    pub fn build_log(&self) -> PathBuf {
        self.dir.join("build.log")
    }

    /// rpmspec --parse stderr log (legacy).
    pub fn rpmspec_log(&self) -> PathBuf {
        self.dir.join("rpmspec.txt")
    }

    /// Directory where the sdist tarball is extracted.
    pub fn sdist_dir(&self) -> PathBuf {
        self.dir.join("sdist")
    }

    /// Append-only attempt journal read by the agent.
    pub fn notes(&self) -> PathBuf {
        self.dir.join("notes.md")
    }

    /// Durable state JSON: last built version, last outcome.
    pub fn state_json(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Create or refresh the `spec` symlink pointing at `spec_path` (absolute).
    #[cfg(unix)]
    pub fn link_spec(&self, spec_path: &Path) -> std::io::Result<()> {
        let link = self.dir.join("spec");
        if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(spec_path, &link)
    }

    /// True when sdist_dir exists and is non-empty.
    pub fn sdist_extracted(&self) -> bool {
        let dir = self.sdist_dir();
        dir.exists()
            && fs::read_dir(&dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
    }

    /// Update `latest` symlink to point at a per-run package directory.
    #[cfg(unix)]
    pub fn link_latest(&self, run_pkg_dir: &Path) -> std::io::Result<()> {
        let link = self.dir.join("latest");
        if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(run_pkg_dir, &link)
    }
}
