//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ap_core::event::Event;

use crate::proc::{self, Emit};

/// Where `osc` runs when the command is not scoped to a checkout.
fn anywhere() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error(transparent)]
    Process(#[from] proc::ProcessError),

    #[error("osc {cmd} failed (exit {code}):\n{stderr}")]
    OscFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("rpmspec --parse {spec} failed (exit {code}):\n{stderr}")]
    RpmspecFailed {
        spec: String,
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

/// OBS remote build status for a single arch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildStatus {
    Succeeded,
    Failed,
    Unresolvable,
    Building,
    Scheduled,
    Blocked,
    Broken,
    Disabled,
    Excluded,
    Unknown(String),
}

impl BuildStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BuildStatus::Succeeded
                | BuildStatus::Failed
                | BuildStatus::Unresolvable
                | BuildStatus::Broken
                | BuildStatus::Disabled
                | BuildStatus::Excluded
        )
    }

    pub fn is_success(&self) -> bool {
        matches!(self, BuildStatus::Succeeded)
    }
}

impl std::fmt::Display for BuildStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuildStatus::Succeeded => "succeeded",
            BuildStatus::Failed => "failed",
            BuildStatus::Unresolvable => "unresolvable",
            BuildStatus::Building => "building",
            BuildStatus::Scheduled => "scheduled",
            BuildStatus::Blocked => "blocked",
            BuildStatus::Broken => "broken",
            BuildStatus::Disabled => "disabled",
            BuildStatus::Excluded => "excluded",
            BuildStatus::Unknown(s) => s.as_str(),
        };
        write!(f, "{s}")
    }
}

impl From<&str> for BuildStatus {
    fn from(s: &str) -> Self {
        match s {
            "succeeded" => BuildStatus::Succeeded,
            "failed" => BuildStatus::Failed,
            "unresolvable" => BuildStatus::Unresolvable,
            "building" => BuildStatus::Building,
            "scheduled" => BuildStatus::Scheduled,
            "blocked" => BuildStatus::Blocked,
            "broken" => BuildStatus::Broken,
            "disabled" => BuildStatus::Disabled,
            "excluded" => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
    }
}

/// Structured classification of why a build failed.
#[derive(Debug)]
pub enum FailureKind {
    NoRepositories,
    NotWorkingCopy,
    UnresolvableDeps(Vec<String>),
    BuildFailed,
}

/// Classify a failed build log into a structured failure kind.
fn classify_build_failure(log: &str) -> FailureKind {
    if log.contains("no repositories defined") {
        return FailureKind::NoRepositories;
    }
    if log.contains("is not a working copy") {
        return FailureKind::NotWorkingCopy;
    }
    let unresolvable = extract_unresolvable(log);
    if !unresolvable.is_empty() {
        return FailureKind::UnresolvableDeps(unresolvable);
    }
    FailureKind::BuildFailed
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

/// Strip the `python<digits>-` or `python-` prefix from an RPM name to get
/// the underlying package name (PyPI name for Python packages, or the bare
/// name for system packages).
pub fn strip_rpm_prefix(rpm: &str) -> &str {
    // Match `python` + optional digits + `-`, e.g. `python313-`, `python3-`, `python-`.
    if let Some(rest) = rpm.strip_prefix("python") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if let Some(name) = rest.strip_prefix('-') {
            return name;
        }
    }
    rpm
}

/// Return the set of path-project names that the OBS project builds against.
///
/// Parses `osc meta prj <project>` XML and extracts every `<path project="..."/>`
/// attribute. These are the upstream projects (e.g. `openSUSE:Factory`) whose
/// packages are considered "already available" and do not need to be built.
pub fn get_project_repo_paths(project: &str, emit: Emit<'_>) -> Vec<String> {
    let Ok(out) = proc::capture("osc", &["meta", "prj", project], &anywhere(), emit) else {
        return vec![];
    };
    if !out.success() {
        return vec![];
    }

    let xml = out.stdout;
    let mut paths = Vec::new();
    for chunk in xml.split("<path ") {
        if let Some(rest) = chunk.strip_prefix("project=\"").or_else(|| {
            // attribute may appear after other attrs, find it
            chunk.find("project=\"").map(|i| &chunk[i + "project=\"".len()..])
        }) {
            if let Some(end) = rest.find('"') {
                let name = rest[..end].to_owned();
                if !name.is_empty() && !paths.contains(&name) {
                    paths.push(name);
                }
            }
        }
    }
    paths
}

/// Return the union of source packages in `project` and all its configured
/// build/path projects (from `osc meta prj`).
///
/// This is the correct set to check for resolvability: a package is available
/// for building if it exists in the user's own project OR in any project that
/// the user's project is configured to build against.
pub fn get_available_packages(
    project: &str,
    emit: Emit<'_>,
) -> std::collections::HashSet<String> {
    let mut all = list_project_packages(project, emit);
    for path_project in get_project_repo_paths(project, emit) {
        all.extend(list_project_packages(&path_project, emit));
    }
    all
}

static PROJECT_PACKAGES_CACHE: OnceLock<Mutex<HashMap<String, std::collections::HashSet<String>>>> =
    OnceLock::new();

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip. Call sites may also pre-fetch by spawning a
/// thread before the result is needed — the cache ensures the second call
/// returns instantly.
pub fn list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> std::collections::HashSet<String> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap();
        if let Some(pkgs) = guard.get(project) {
            return pkgs.clone();
        }
    }

    let pkgs = fetch_project_packages(project, emit);
    cache.lock().unwrap().insert(project.to_owned(), pkgs.clone());
    pkgs
}

fn fetch_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> std::collections::HashSet<String> {
    let Ok(out) = proc::capture("osc", &["ls", project], &anywhere(), emit) else {
        return Default::default();
    };
    if !out.success() {
        return Default::default();
    }
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Create an initial `.changes` entry for a new package using `osc vc`.
///
/// Only runs when the `.changes` file does not yet exist. `osc vc` reads the
/// committer identity from `~/.config/osc/oscrc` and writes a datestamped
/// entry at the top of the file.
pub fn create_initial_changes(
    checkout_dir: &Path,
    pkg_name: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let changes_name = format!("{pkg_name}.changes");
    if checkout_dir.join(&changes_name).exists() {
        return Ok(());
    }
    let out = proc::capture("osc", &["vc", "-m", "Initial packaging"], checkout_dir, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: "vc -m \"Initial packaging\"".to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ensure the package exists in `project`, creating it if not.
///
/// `osc mkpac <pkg>` must be run from inside a project checkout directory,
/// so we check out the project first if needed, then run mkpac from there.
pub fn ensure_package(
    project: &str,
    pkg_name: &str,
    work_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = proc::capture(
        "osc",
        &["ls", &format!("{project}/{pkg_name}")],
        work_dir,
        emit,
    )?;

    if check.success() {
        return Ok(());
    }

    // Ensure the project directory is a valid osc working copy (.osc/ present).
    // A plain directory is not enough — osc mkpac requires osc metadata.
    // Check for .osc/ specifically so we don't re-run co when already set up.
    let project_dir = work_dir.join(project);
    if !project_dir.join(".osc").exists() {
        let out = proc::capture("osc", &["co", project, "-M"], work_dir, emit)?;

        // A concurrent thread may have created it between our check and
        // the co call. If .osc/ now exists we're good; otherwise fail.
        if !out.success() && !project_dir.join(".osc").exists() {
            return Err(Error::OscFailed {
                cmd: format!("co {project} -M"),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }

    // Create the package inside the project checkout.
    let out = proc::capture("osc", &["mkpac", pkg_name], &project_dir, emit)?;

    if !out.success() {
        // Either case means the local directory is already valid — continue.
        if out.stderr.contains("already exists")
            || out.stderr.contains("is already under version control")
        {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("mkpac {pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(())
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(
    project: &str,
    pkg_name: &str,
    into_dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        let out = proc::capture("osc", &["up"], &checkout_dir, emit)?;

        if !out.success() {
            // 404 means the package was just created locally by `osc mkpac`
            // but not yet committed to the server — use the directory as-is.
            if out.stderr.contains("404") || out.stderr.contains("Not Found") {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: "up".into(),
                code: out.code,
                stderr: out.stderr,
            });
        }

        return Ok(checkout_dir);
    }

    let out = proc::capture(
        "osc",
        &["co", &format!("{project}/{pkg_name}")],
        into_dir,
        emit,
    )?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(checkout_dir)
}

/// Run `osc up` from `checkout_dir` to update the working copy.
///
/// Non-fatal: an already-up-to-date or brand-new checkout exits non-zero; we
/// ignore those cleanly.
pub fn pull(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["up"], checkout_dir, emit)?;
    if !out.success() {
        // 404 means the package was created locally via `osc mkpac` but not
        // yet committed — the directory is valid, nothing to pull.
        let benign = ["nothing to update", "At revision", "Skipping", "404", "Not Found"];
        if !benign.iter().any(|b| out.stdout.contains(b) || out.stderr.contains(b)) {
            return Err(Error::OscFailed {
                cmd: "up".to_string(),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Run `osc service ra` from `checkout_dir` to fetch sources and vendor Cargo deps.
pub fn run_service(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["service", "ra"], checkout_dir, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: "service ra".to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Run `rpmspec --parse` on a spec file and check for syntax errors.
///
/// Judges on exit status only (warnings in stderr are not treated as failures).
/// Returns the expanded spec on success (stdout from `rpmspec --parse`).
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    let out = proc::capture(
        "rpmspec",
        &["--parse", &spec_path.to_string_lossy()],
        &anywhere(),
        emit,
    )?;

    if !out.success() {
        return Err(Error::RpmspecFailed {
            spec: spec_path.display().to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(out.stdout)
}

/// Commit the package to OBS, triggering a remote build.
///
/// Runs `osc addremove` first to track any new or deleted files, then
/// `osc ci` to push the changes to OBS.
pub fn commit(checkout_dir: &Path, message: &str, emit: Emit<'_>) -> Result<(), Error> {
    // Sync tracked files with directory contents (ignore errors — may be a new pkg).
    let _ = proc::capture("osc", &["addremove"], checkout_dir, emit);

    let out = proc::capture("osc", &["ci", "-m", message], checkout_dir, emit)?;

    if !out.success() {
        // "nothing to do" means files are already committed — not an error.
        if out.stdout.contains("nothing to do") || out.stderr.contains("nothing to do") {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("ci -m {message:?}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Query the OBS build status for one repo/arch combination.
///
/// Runs `osc results -r <repo> -a <arch> <project> <package>` and parses
/// the last whitespace-separated token on the first result line as the status.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    let out = proc::capture(
        "osc",
        &["results", "-r", repo, "-a", arch, project, pkg_name],
        &anywhere(),
        emit,
    )?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("results -r {repo} -a {arch} {project} {pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    for line in out.stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            return Ok(BuildStatus::from(parts[parts.len() - 1]));
        }
    }
    Ok(BuildStatus::Unknown("no results".to_string()))
}

/// Fetch the OBS build log into `log_path`.
///
/// Runs `osc buildlog <project> <package> <repo> <arch>` and writes
/// stdout to `log_path`. Returns `Ok` even when the exit code is non-zero
/// if some output was captured (partial logs are still useful).
pub fn fetch_buildlog(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let out = proc::capture(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
    )?;

    std::fs::write(log_path, &out.stdout)?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Download built binaries from OBS into `dest_dir`.
///
/// Runs `osc getbinaries --destdir=<dest> <project> <package> <repo> <arch>`.
/// Errors are non-fatal: the caller logs a warning and continues.
pub fn get_binaries(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    dest_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let _ = fs::create_dir_all(dest_dir);
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture(
        "osc",
        &[
            "getbinaries",
            &format!("--destdir={dest}"),
            project,
            pkg_name,
            repo,
            arch,
        ],
        &anywhere(),
        emit,
    )?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Try to extract unresolvable package names from `osc results --verbose`.
///
/// Used as a fallback when the build log doesn't contain "nothing provides" lines.
fn get_unresolvable_from_results(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Vec<String> {
    let Ok(out) = proc::capture(
        "osc",
        &["results", "--verbose", "-r", repo, "-a", arch, project, pkg_name],
        &anywhere(),
        emit,
    ) else {
        return vec![];
    };
    extract_unresolvable(&out.stdout)
}

/// Wait for the OBS remote build to complete, fetch the log, and download
/// built RPMs on success.
///
/// The caller must commit the package to OBS before calling this (see `commit`).
/// Polls `osc results` every 15 seconds, up to 30 minutes. Emits a `Warning`
/// event each time the build status changes so the user can see progress.
pub fn obs_build(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let reporter = emit.reporter;
    let step = emit.step;
    const POLL_INTERVAL: Duration = Duration::from_secs(15);
    const MAX_WAIT: Duration = Duration::from_secs(1800);

    let start = Instant::now();
    let mut prev_status = String::new();

    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "results".to_string(),
            "--watch".to_string(),
            "-r".to_string(),
            repo.to_string(),
            "-a".to_string(),
            arch.to_string(),
            project.to_string(),
            pkg_name.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });

    let status = loop {
        if start.elapsed() >= MAX_WAIT {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s waiting for OBS build",
                    MAX_WAIT.as_secs()
                ),
            });
        }

        let status = get_build_status(project, pkg_name, repo, arch, Emit::quiet())?;
        let status_str = status.to_string();

        if status_str != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {status_str}"),
            });
            prev_status = status_str;
        }

        if status.is_terminal() {
            break status;
        }

        std::thread::sleep(POLL_INTERVAL);
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    let _ = fetch_buildlog(project, pkg_name, repo, arch, log_path, Emit::quiet());
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download built RPMs so prefer-pkgs works for dependent packages.
        if let Err(e) = get_binaries(project, pkg_name, repo, arch, rpms_dir, emit) {
            reporter.report(&Event::Warning {
                step,
                message: format!("getbinaries failed (non-fatal): {e}"),
            });
        }
        reporter.report(&Event::CommandDone {
            step,
            code: 0,
            elapsed,
            stdout_bytes: log_bytes,
            stderr_bytes: 0,
        });
        return Ok(());
    }

    reporter.report(&Event::CommandDone {
        step,
        code: 1,
        elapsed,
        stdout_bytes: log_bytes,
        stderr_bytes: 0,
    });

    let log_text = fs::read_to_string(log_path).unwrap_or_default();

    Err(match status {
        BuildStatus::Unresolvable => {
            let mut pkgs = extract_unresolvable(&log_text);
            if pkgs.is_empty() {
                pkgs = get_unresolvable_from_results(project, pkg_name, repo, arch, emit);
            }
            Error::UnresolvableDeps(pkgs)
        }
        BuildStatus::Broken => {
            Error::NotActionable("OBS: package source is broken".to_string())
        }
        BuildStatus::Disabled => {
            Error::NotActionable("OBS: build is disabled in project config".to_string())
        }
        BuildStatus::Excluded => {
            Error::NotActionable("OBS: build is excluded in project config".to_string())
        }
        _ => match classify_build_failure(&log_text) {
            FailureKind::NoRepositories => Error::NotActionable(
                "OBS project has no build repositories — configure them in OBS first".to_string(),
            ),
            FailureKind::NotWorkingCopy => Error::NotActionable(
                "osc checkout is not server-backed".to_string(),
            ),
            FailureKind::UnresolvableDeps(pkgs) => Error::UnresolvableDeps(pkgs),
            FailureKind::BuildFailed => Error::BuildFailed {
                spec: format!("{pkg_name}.spec"),
                code: 1,
                log: log_path.to_owned(),
            },
        },
    })
}
