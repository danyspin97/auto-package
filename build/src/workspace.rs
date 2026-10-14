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
    /// The link is swapped in one rename, so a reader never sees it missing and
    /// a failure leaves the previous link in place.
    #[cfg(unix)]
    pub fn link_spec(&self, spec_path: &Path) -> std::io::Result<()> {
        ap_core::fsx::replace_symlink(spec_path, &self.dir.join("spec"))
    }

    /// Records which tarball `sdist_dir` was extracted from.
    fn sdist_stamp(&self) -> PathBuf {
        self.dir.join("sdist.stamp")
    }

    /// `name size` of a tarball, the identity the stamp records.
    fn tarball_identity(tarball: &Path) -> Option<String> {
        let name = tarball.file_name()?.to_string_lossy().into_owned();
        let len = fs::metadata(tarball).ok()?.len();
        Some(format!("{name} {len}"))
    }

    /// True when sdist_dir holds a completed extraction of this very tarball.
    /// A different tarball (version bump, regenerated source) or an extraction
    /// that never finished (no stamp) reads as not extracted.
    pub fn sdist_extracted_from(&self, tarball: &Path) -> bool {
        let dir = self.sdist_dir();
        let non_empty = fs::read_dir(&dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        non_empty
            && Self::tarball_identity(tarball)
                .is_some_and(|id| fs::read_to_string(self.sdist_stamp()).is_ok_and(|s| s == id))
    }

    /// Remove any previous extraction so a new one starts from an empty dir.
    pub fn clear_sdist(&self) -> std::io::Result<()> {
        let _ = fs::remove_file(self.sdist_stamp());
        match fs::remove_dir_all(self.sdist_dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Record a finished extraction of `tarball` into `sdist_dir`.
    pub fn mark_sdist_extracted(&self, tarball: &Path) -> std::io::Result<()> {
        match Self::tarball_identity(tarball) {
            Some(id) => fs::write(self.sdist_stamp(), id),
            None => Ok(()),
        }
    }

    /// Update `latest` symlink to point at a per-run package directory, in one
    /// rename so a failure leaves the previous link in place.
    #[cfg(unix)]
    pub fn link_latest(&self, run_pkg_dir: &Path) -> std::io::Result<()> {
        ap_core::fsx::replace_symlink(run_pkg_dir, &self.dir.join("latest"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdist_is_extracted_only_for_the_tarball_it_came_from() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path(), "python-foo").unwrap();
        let old = tmp.path().join("foo-1.0.tar.gz");
        let new = tmp.path().join("foo-1.1.tar.gz");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"newer").unwrap();

        assert!(!ws.sdist_extracted_from(&old));
        fs::create_dir_all(ws.sdist_dir().join("foo-1.0")).unwrap();
        // Extraction not finished: no stamp yet.
        assert!(!ws.sdist_extracted_from(&old));
        ws.mark_sdist_extracted(&old).unwrap();
        assert!(ws.sdist_extracted_from(&old));
        assert!(!ws.sdist_extracted_from(&new));

        ws.clear_sdist().unwrap();
        assert!(!ws.sdist_dir().exists());
        assert!(!ws.sdist_extracted_from(&old));
        ws.clear_sdist().unwrap();
    }
}
