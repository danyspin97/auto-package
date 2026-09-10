//! Local `osc build` driver.
//!
//! Each package is built in its own subdirectory. Already-built RPMs from
//! earlier packages are passed via `--prefer-pkgs` so dependencies satisfy
//! each other without a network round-trip to OBS.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error("osc build failed for {spec} (exit {code}); log at {log}")]
    BuildFailed {
        spec: String,
        code: i32,
        log: PathBuf,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Build `spec_path` with `osc build`.
///
/// `pkg_dir` must already contain the spec and all sources. `rpms_dir` is
/// passed as `--prefer-pkgs` so that previously built packages in this run
/// are visible to the build root.
///
/// Returns the path to the build log on failure so the caller can hand it to
/// the log extractor and the AI fix loop.
pub fn build(
    pkg_dir: &Path,
    spec_path: &Path,
    rpms_dir: &Path,
    distro: &str,
    arch: &str,
) -> Result<(), Error> {
    let log_path = pkg_dir.join("build.log");

    let status = Command::new("osc")
        .args([
            "build",
            "--vm-type=kvm",
            "--prefer-pkgs",
            rpms_dir.to_str().unwrap(),
            "--keep-pkgs",
            rpms_dir.to_str().unwrap(),
            distro,
            arch,
            spec_path.to_str().unwrap(),
        ])
        .current_dir(pkg_dir)
        .stdout(std::fs::File::create(&log_path).map_err(Error::Io)?)
        .stderr(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .map_err(Error::Io)?,
        )
        .status()
        .map_err(Error::Launch)?;

    if !status.success() {
        return Err(Error::BuildFailed {
            spec: spec_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            code: status.code().unwrap_or(-1),
            log: log_path,
        });
    }

    Ok(())
}
