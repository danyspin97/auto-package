//! Building on OBS with retries: the retriggers of broken builds, the fetch of the
//! built RPMs and the rpmlint logs, and the build log tail.

use super::*;
use crate::osc::build_once::*;

/// The end of a package's latest build log for `repo`/`arch`, where the
/// rpmlint block is, or `None` when OBS has no log for it.
pub fn fetch_build_log_tail(project: &str, pkg: &str, repo: &str, arch: &str) -> Option<String> {
    const KEEP: usize = 1 << 20;
    check_obs_name("project", project).ok()?;
    check_obs_name("package", pkg).ok()?;
    let log = osc_api(&format!(
        "/build/{}/{}/{}/{}/_log?nostream=1&start=0",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg)
    ))?;
    if log.len() <= KEEP {
        return Some(log);
    }
    let mut from = log.len() - KEEP;
    while !log.is_char_boundary(from) {
        from += 1;
    }
    Some(log[from..].to_string())
}

/// Wait for the OBS remote build to complete, fetch the log, and download
/// built RPMs on success.
///
/// The caller must commit the package to OBS before calling this (see `commit`).
/// Polls the OBS build status (every 15 seconds, less often while it holds
/// still; see [`poll_delay`]), for at most two hours (the
/// `MAX_WAIT` below), then gives up with a timeout error. Emits a `Progress`
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
    build_with_retriggers(
        pkg_name,
        emit,
        BROKEN_RETRIGGER_PAUSE,
        || obs_build_once(project, pkg_name, repo, arch, log_path, rpms_dir, emit),
        || trigger_rebuild(project, pkg_name, repo, arch, emit),
    )
}

/// The retry loop of [`obs_build`], with the build and the re-trigger injected:
/// a `broken` verdict with no build log behind it is re-triggered up to
/// [`MAX_BROKEN_RETRIGGERS`] times, then stands as a plain `broken`. Any other
/// result is returned as it came.
pub(super) fn build_with_retriggers(
    pkg_name: &str,
    emit: Emit<'_>,
    pause: Duration,
    mut once: impl FnMut() -> Result<(), Error>,
    mut rebuild: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    let mut retriggers = 0;
    loop {
        match once() {
            Err(Error::NotActionable(msg)) if msg == BROKEN_UNCONFIRMED => {
                if retriggers >= MAX_BROKEN_RETRIGGERS {
                    return Err(Error::NotActionable(BROKEN_MESSAGE.to_string()));
                }
                retriggers += 1;
                emit.reporter.report(&Event::Warning {
                    step: emit.step,
                    message: format!(
                        "OBS says {pkg_name} is broken but has no build log to confirm it; \
                         treating it as transient and re-triggering the build \
                         ({retriggers}/{MAX_BROKEN_RETRIGGERS})"
                    ),
                });
                if let Err(e) = rebuild() {
                    emit.reporter.report(&Event::Warning {
                        step: emit.step,
                        message: format!("could not re-trigger the OBS build: {e}"),
                    });
                }
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
            other => return other,
        }
    }
}

/// The final message for a `broken` verdict.
pub(super) const BROKEN_MESSAGE: &str = "OBS: package source is broken";

/// Internal marker: `broken` without a build log behind it (see [`obs_build`]).
pub(super) const BROKEN_UNCONFIRMED: &str =
    "OBS: broken status could not be confirmed by a build log";

/// How often an unconfirmed `broken` verdict is re-triggered before it stands.
pub(super) const MAX_BROKEN_RETRIGGERS: u32 = 3;

const BROKEN_RETRIGGER_PAUSE: Duration = Duration::from_secs(30);

/// Whether a `broken` verdict is backed by evidence: a non-empty build log.
pub(super) fn broken_is_confirmed(log_path: &Path) -> bool {
    read_log_lossy(log_path).is_ok_and(|t| !t.trim().is_empty())
}

/// A build log as text, lossily: a log with one invalid UTF-8 byte is still
/// the evidence it was, and must not read as an empty or missing file.
pub(super) fn read_log_lossy(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The text of a build log that is the evidence for a failure. One that
/// cannot be read, or is empty (a failed `osc buildlog` leaves an empty file
/// behind), is an environment problem, reported as an I/O error.
pub(super) fn usable_build_log(log_path: &Path) -> Result<String, Error> {
    read_log_lossy(log_path)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            if text.trim().is_empty() {
                Err("the file is empty".to_string())
            } else {
                Ok(text)
            }
        })
        .map_err(|why| {
            Error::Io(std::io::Error::other(format!(
                "no usable build log at {}: {why}",
                log_path.display()
            )))
        })
}

/// Ask OBS to rebuild `pkg_name` (`osc rebuild`), which re-evaluates a source
/// it had flagged `broken`.
fn trigger_rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Call `f` up to `attempts` times, pausing `pause` between failures.
pub(super) fn retry<T, E>(
    attempts: u32,
    pause: Duration,
    mut f: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    let mut n = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if n >= attempts => return Err(e),
            Err(_) => {
                n += 1;
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
        }
    }
}

/// `osc getbinaries` right after a green verdict can fail transiently.
const GETBINARIES_ATTEMPTS: u32 = 4;

const GETBINARIES_PAUSE: Duration = Duration::from_secs(20);

/// One package's `getbinaries`, `get` and the `disk` gate injected: retried up
/// to `attempts` times, and each try first runs `disk` (a full disk fails the
/// try with code 28 without running `get`).
#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_with_retries(
    project: &str,
    name: &str,
    repo: &str,
    arch: &str,
    attempts: u32,
    pause: Duration,
    mut disk: impl FnMut() -> Result<(), String>,
    mut get: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    retry(attempts, pause, || {
        if let Err(stderr) = disk() {
            return Err(Error::OscFailed {
                cmd: format!("getbinaries {project} {name} {repo} {arch}"),
                code: 28,
                stderr,
            });
        }
        get()
    })
}

/// Download the RPMs of `pkg_name` (and its multibuild flavors) into
/// `rpms_dir`, retrying a failing `osc getbinaries`. Returns how many of the
/// RPMs OBS lists landed.
pub fn download_built_rpms(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<usize, Error> {
    let mut packages = vec![pkg_name.to_string()];
    packages.extend(multibuild_flavors(project, pkg_name, repo, arch));
    let mut downloaded_count = 0;
    for name in &packages {
        let listing = binary_rpm_listing(project, name, repo, arch);
        if let Some(names) = &listing {
            remove_superseded_rpms(rpms_dir, names);
        }
        // A flavor `pkg:flavor` ships RPMs named `pkg-flavor-<version>…`.
        let file_prefix = name.replace(':', "-");
        let started = SystemTime::now();
        fetch_with_retries(
            project,
            name,
            repo,
            arch,
            GETBINARIES_ATTEMPTS,
            GETBINARIES_PAUSE,
            || crate::workspace::await_disk_space(rpms_dir, emit.reporter, emit.step),
            || get_binaries(project, name, repo, arch, rpms_dir, emit),
        )?;
        stash_rpmlint_log(rpms_dir, name);
        downloaded_count +=
            downloaded_rpm_count(listing.as_deref(), rpms_dir, &file_prefix, started);
    }
    Ok(downloaded_count)
}

/// Directory under `rpms_dir` holding the `rpmlint.log` OBS published for each
/// package (and multibuild flavor) downloaded, until the pipeline records it.
const RPMLINT_STASH: &str = ".rpmlint";

/// `osc getbinaries` writes OBS's `rpmlint.log` next to the RPMs, where the next
/// package's download overwrites it. Move it to a file of its own for `name`.
pub(super) fn stash_rpmlint_log(rpms_dir: &Path, name: &str) {
    let log = rpms_dir.join("rpmlint.log");
    if !log.is_file() {
        return;
    }
    let dir = rpms_dir.join(RPMLINT_STASH);
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::rename(&log, dir.join(format!("{name}.log")));
    }
}

/// The stashed rpmlint logs of `pkg` and its multibuild flavors
/// (`pkg:flavor`), as `(name, text)`, removed from the stash.
pub fn take_rpmlint_logs(rpms_dir: &Path, pkg: &str) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(rpms_dir.join(RPMLINT_STASH)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".log") else {
            continue;
        };
        if (name == pkg || name.strip_prefix(pkg).is_some_and(|r| r.starts_with(':')))
            // Bytes, lossily: a log with one invalid byte is still a log, and a
            // file that never reads would stay in the stash for ever.
            && let Ok(bytes) = fs::read(e.path())
        {
            let _ = fs::remove_file(e.path());
            out.push((
                name.to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
    }
    out.sort();
    out
}

/// The error lines of an rpmlint log (`pkg.rpm: E: rule detail`).
pub fn rpmlint_errors(log: &str) -> Vec<String> {
    log.lines()
        .filter(|l| l.contains(": E: "))
        .map(|l| l.trim().to_string())
        .collect()
}
