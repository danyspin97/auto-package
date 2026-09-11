//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

    /// Build failed for a reason that no agent fix can address.
    /// The caller should stop immediately and report the message.
    #[error("not actionable: {0}")]
    NotActionable(String),

    /// One or more RPM dependencies could not be resolved. The inner vector
    /// holds the raw RPM package names (e.g. `python314-puccinialin`).
    /// The caller should queue those packages for building and retry this one.
    #[error("unresolvable RPM dependencies: {}", .0.join(", "))]
    UnresolvableDeps(Vec<String>),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Extract RPM package names from "nothing provides <pkg>" lines in a build log.
///
/// Handles both bare `nothing provides <pkg>` and `unresolvable: nothing provides <pkg>`,
/// strips any trailing `needed by ...` clause, and deduplicates.
fn extract_unresolvable(log: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for raw_line in log.lines() {
        let line = raw_line.trim();
        let pkg_str = if let Some(rest) = line.strip_prefix("unresolvable: nothing provides ") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("nothing provides ") {
            rest.trim()
        } else {
            continue;
        };
        // Take just the package name; drop " needed by ..." suffix.
        let pkg = pkg_str
            .split_whitespace()
            .next()
            .unwrap_or(pkg_str);
        if !pkg.is_empty() && seen.insert(pkg.to_string()) {
            result.push(pkg.to_string());
        }
    }
    result
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
        .stdin(Stdio::null())
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
            .stdin(Stdio::null())
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
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // "already exists" means a previous mkpac ran but was never committed —
        // the local directory is valid, just continue.
        if stderr.contains("already exists") {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("mkpac {pkg_name}"),
            code: out.status.code().unwrap_or(-1),
            stderr: stderr.into_owned(),
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
            .stdin(Stdio::null())
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
        .stdin(Stdio::null())
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

/// Run `rpmspec --parse` on a spec file and check for syntax errors or unexpanded macros.
///
/// Returns `Err` with the relevant stderr output when the spec is invalid.
pub fn rpmspec_check(spec_path: &Path) -> Result<(), Error> {
    let out = Command::new("rpmspec")
        .args(["--parse", spec_path.to_str().unwrap_or_default()])
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    let stderr = String::from_utf8_lossy(&out.stderr);

    if !out.status.success() || stderr.contains("warning:") {
        return Err(Error::OscFailed {
            cmd: format!("rpmspec --parse {}", spec_path.display()),
            code: out.status.code().unwrap_or(-1),
            stderr: stderr.into_owned(),
        });
    }

    Ok(())
}

/// Locate the BUILDROOT directory left behind by a failed chroot build.
///
/// For chroot builds, osc leaves the build root at
/// `/var/tmp/build-root/<distro>-<arch>/`. The RPM BUILDROOT is nested inside
/// at `…/home/abuild/rpmbuild/BUILDROOT/<name>-<ver>-<rel>.<arch>/`.
///
/// Returns the first matching BUILDROOT directory found, or `None` if the build
/// root is absent (e.g. because a kvm build unmounts it on exit).
pub fn find_buildroot(distro: &str, arch: &str) -> Option<PathBuf> {
    let build_root = PathBuf::from(format!("/var/tmp/build-root/{distro}-{arch}"));
    if !build_root.exists() {
        return None;
    }
    // Walk into home/abuild/rpmbuild/BUILDROOT/ and return the first entry.
    let br_parent = build_root.join("home/abuild/rpmbuild/BUILDROOT");
    if let Ok(mut entries) = fs::read_dir(&br_parent) {
        if let Some(Ok(entry)) = entries.next() {
            let path = entry.path();
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    None
}

/// Run `osc build` from `checkout_dir`.
///
/// `spec_name` is the filename of the spec (e.g. `python-foo.spec`); passing
/// it explicitly avoids a nondeterministic `read_dir` scan.
///
/// `vm_type` controls the build isolation (`"chroot"` or `"kvm"`). Chroot is
/// faster locally and leaves the BUILDROOT accessible after a failure.
///
/// `log_path` is where stdout and stderr of the build are written.  Pass a
/// path inside the workspace rather than the checkout directory so `osc status`
/// remains clean.
///
/// Built RPMs are also placed in `rpms_dir` via `--keep-pkgs` so that later
/// packages in the same run can use them as `--prefer-pkgs`.
pub fn build(
    checkout_dir: &Path,
    spec_name: &str,
    vm_type: &str,
    log_path: &Path,
    rpms_dir: &Path,
    distro: &str,
    arch: &str,
) -> Result<(), Error> {
    let vm_arg = format!("--vm-type={vm_type}");
    let status = Command::new("osc")
        .args([
            "build",
            &vm_arg,
            "--prefer-pkgs",
            rpms_dir.to_str().unwrap(),
            "--keep-pkgs",
            rpms_dir.to_str().unwrap(),
            distro,
            arch,
            spec_name,
        ])
        .current_dir(checkout_dir)
        .stdin(Stdio::null())
        .stdout(fs::File::create(log_path).map_err(Error::Io)?)
        .stderr(
            std::fs::OpenOptions::new()
                .append(true)
                .open(log_path)
                .map_err(Error::Io)?,
        )
        .status()
        .map_err(Error::Launch)?;

    if !status.success() {
        let log_text = fs::read_to_string(log_path).unwrap_or_default();

        // Detect errors that no spec fix can address.
        if log_text.contains("no repositories defined") {
            return Err(Error::NotActionable(
                "OBS project has no build repositories — configure them in OBS first".to_string(),
            ));
        }
        if log_text.contains("is not a working copy") {
            return Err(Error::NotActionable(
                "osc checkout directory is not server-backed — run 'osc ci' in the package dir first".to_string(),
            ));
        }

        let unresolvable = extract_unresolvable(&log_text);
        if !unresolvable.is_empty() {
            return Err(Error::UnresolvableDeps(unresolvable));
        }

        return Err(Error::BuildFailed {
            spec: spec_name.to_owned(),
            code: status.code().unwrap_or(-1),
            log: log_path.to_owned(),
        });
    }

    Ok(())
}

/// Prepare source using quilt: unpack tarball and apply all patches.
///
/// Runs `quilt setup <spec>` to unpack the source tarball, then `quilt push -a`
/// to apply all patches in series. Returns the path to the unpacked source
/// directory (typically `<pkg_dir>/<pkg>-<version>/`).
///
/// `pkg_dir` is the package directory containing the spec file and tarballs.
/// `spec_name` is the spec filename (e.g. `python-foo.spec`).
///
/// This must be called from `pkg_dir` as the working directory, since quilt
/// setup creates directories relative to the current location.
pub fn quilt_prepare_source(pkg_dir: &Path, spec_name: &str) -> Result<PathBuf, Error> {
    // Run quilt setup to unpack source and create patch series.
    let setup = Command::new("quilt")
        .args(["setup", spec_name])
        .current_dir(pkg_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    if !setup.status.success() {
        return Err(Error::OscFailed {
            cmd: format!("quilt setup {spec_name}"),
            code: setup.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&setup.stderr).into_owned(),
        });
    }

    // Apply all patches.
    let push = Command::new("quilt")
        .args(["push", "-a"])
        .current_dir(pkg_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(Error::Launch)?;

    // quilt push -a exits non-zero when there are no patches, but that's fine.
    // Check stderr for actual errors instead.
    let stderr = String::from_utf8_lossy(&push.stderr);
    if !push.status.success() && !stderr.contains("No patches in series") {
        return Err(Error::OscFailed {
            cmd: "quilt push -a".to_string(),
            code: push.status.code().unwrap_or(-1),
            stderr: stderr.into_owned(),
        });
    }

    // Find the unpacked source directory. Quilt creates it in the current dir.
    // It's typically named <pkg>-<version>/ matching the tarball top-level.
    let source_dir = fs::read_dir(pkg_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            // Skip known non-source directories and files.
            if path.is_dir()
                && !name.starts_with('.')
                && name != "patches"
                && !name.ends_with(".spec")
            {
                Some(path)
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| {
            Error::OscFailed {
                cmd: "find source dir".to_string(),
                code: -1,
                stderr: "could not find unpacked source directory after quilt setup".to_string(),
            }
        })?;

    Ok(source_dir)
}
