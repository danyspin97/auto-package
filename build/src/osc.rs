//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error("osc {cmd} failed (exit {code}):\n{stderr}")]
    OscFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("osc build failed for {spec} (exit {code}); log at {log}")]
    BuildFailed {
        spec: String,
        code: i32,
        log: PathBuf,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// RPM package name for a Python package: `python-<name>` (lowercased).
pub fn rpm_name(pypi_name: &str) -> String {
    format!("python-{}", pypi_name.to_lowercase())
}

/// Ensure the package exists in `project`, creating it if not.
pub fn ensure_package(project: &str, pkg_name: &str) -> Result<(), Error> {
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = Command::new("osc")
        .args(["ls", &format!("{project}/{pkg_name}")])
        .output()
        .map_err(Error::Launch)?;

    if check.status.success() {
        return Ok(());
    }

    // Package does not exist — create it.
    let out = Command::new("osc")
        .args(["mkpac", project, pkg_name])
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        return Err(Error::OscFailed {
            cmd: format!("mkpac {project} {pkg_name}"),
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    Ok(())
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(project: &str, pkg_name: &str, into_dir: &Path) -> Result<PathBuf, Error> {
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        // Update the existing checkout.
        let out = Command::new("osc")
            .args(["up"])
            .current_dir(&checkout_dir)
            .output()
            .map_err(Error::Launch)?;

        if !out.status.success() {
            return Err(Error::OscFailed {
                cmd: "up".into(),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }

        return Ok(checkout_dir);
    }

    let out = Command::new("osc")
        .args(["co", &format!("{project}/{pkg_name}")])
        .current_dir(into_dir)
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    Ok(checkout_dir)
}

/// Copy spec and source tarball into an osc checkout directory.
///
/// Removes any stale sources already in the checkout so the directory only
/// contains the files for the current build.
pub fn sync_sources(checkout_dir: &Path, spec_path: &Path, tarball: &Path) -> Result<(), Error> {
    // Remove old sources (skip .osc/ metadata).
    for entry in fs::read_dir(checkout_dir).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let name = entry.file_name();
        if name == ".osc" {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(&path).map_err(Error::Io)?;
        }
    }

    let spec_dest = checkout_dir.join(spec_path.file_name().unwrap());
    fs::copy(spec_path, &spec_dest).map_err(Error::Io)?;

    let tarball_dest = checkout_dir.join(tarball.file_name().unwrap());
    fs::copy(tarball, &tarball_dest).map_err(Error::Io)?;

    Ok(())
}

/// Run `osc build` from `checkout_dir`.
///
/// Built RPMs are also placed in `rpms_dir` via `--keep-pkgs` so that later
/// packages in the same run can use them as `--prefer-pkgs`.
pub fn build(checkout_dir: &Path, rpms_dir: &Path, distro: &str, arch: &str) -> Result<(), Error> {
    let log_path = checkout_dir.join("build.log");

    let spec_name = fs::read_dir(checkout_dir)
        .map_err(Error::Io)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".spec"))
        .unwrap_or_default();

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
            &spec_name,
        ])
        .current_dir(checkout_dir)
        .stdout(fs::File::create(&log_path).map_err(Error::Io)?)
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
            spec: spec_name,
            code: status.code().unwrap_or(-1),
            log: log_path,
        });
    }

    Ok(())
}
