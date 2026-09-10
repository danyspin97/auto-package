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
///
/// `osc mkpac <pkg>` must be run from inside a project checkout directory,
/// so we check out the project first if needed, then run mkpac from there.
pub fn ensure_package(project: &str, pkg_name: &str, work_dir: &Path) -> Result<(), Error> {
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = Command::new("osc")
        .args(["ls", &format!("{project}/{pkg_name}")])
        .output()
        .map_err(Error::Launch)?;

    if check.status.success() {
        return Ok(());
    }

    // Check out the project (top-level, no package) if not already present.
    let project_dir = work_dir.join(project);
    if !project_dir.exists() {
        let out = Command::new("osc")
            .args(["co", project, "-M"])
            .current_dir(work_dir)
            .output()
            .map_err(Error::Launch)?;

        if !out.status.success() {
            return Err(Error::OscFailed {
                cmd: format!("co {project} -M"),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    // Create the package inside the project checkout.
    let out = Command::new("osc")
        .args(["mkpac", pkg_name])
        .current_dir(&project_dir)
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        return Err(Error::OscFailed {
            cmd: format!("mkpac {pkg_name}"),
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
        let out = Command::new("osc")
            .args(["up"])
            .current_dir(&checkout_dir)
            .output()
            .map_err(Error::Launch)?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // 404 means the package was just created locally by `osc mkpac`
            // but not yet committed to the server — use the directory as-is.
            if stderr.contains("404") || stderr.contains("Not Found") {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: "up".into(),
                code: out.status.code().unwrap_or(-1),
                stderr: stderr.into_owned(),
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
