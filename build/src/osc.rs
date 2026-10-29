//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

mod branch;
pub use branch::*;
mod names;
pub use names::*;
mod error;
pub use error::*;

mod failure;
#[cfg(test)]
mod transient_text_tests;
pub use failure::*;

#[cfg(test)]
mod extract_unresolvable_tests;

#[cfg(test)]
mod fuzzy_key_tests;

mod distro;
#[cfg(test)]
mod strip_devel_wrapping_tests;
pub use distro::*;

#[cfg(test)]
mod distro_provides_tests;
mod repo_paths;
pub use repo_paths::*;
mod published;
pub use published::*;

#[cfg(test)]
mod tracked_stray_tests;

#[cfg(test)]
mod pending_changes_tests;

mod project_packages;
#[cfg(test)]
mod published_binary_tests;
pub use project_packages::*;

mod files;
#[cfg(test)]
mod project_packages_cache_tests;
pub use files::*;

mod changes;
#[cfg(test)]
mod spec_file_name_tests;
pub use changes::*;
mod checkout;
pub use checkout::*;

#[cfg(test)]
mod failing_result_tests;

#[cfg(test)]
mod respelling_tests;

#[cfg(test)]
mod osc_error_text_tests;

#[cfg(test)]
mod unfinished_checkout_tests;

#[cfg(test)]
mod sandbox_spec_dir_tests;

#[cfg(test)]
mod xml_attr_and_status_tests;

#[cfg(test)]
mod changes_conflict_tests;

#[cfg(test)]
mod not_found_tests;
mod service;
pub use service::*;
mod sandbox;
pub use sandbox::*;
mod publish;
pub use publish::*;
mod baseline;
pub use baseline::*;

#[cfg(test)]
mod baseline_tests;
mod status;
pub use status::*;
mod query;
use query::*;

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
mod results;
use results::*;

#[cfg(test)]
mod results_cache_tests;

#[cfg(test)]
mod wait_pacing_tests;

mod binaries;
#[cfg(test)]
mod single_build_status_tests;
pub use binaries::*;
mod unresolvable;
pub use unresolvable::*;

/// The name `dep` has in `listing` when the project has it built
/// (`succeeded`) in the repository/architecture `statuses` was read for, else
/// `None`. Names are matched by [`fuzzy_key`], with or without the `python-`
/// prefix, and the listing's own spelling is returned.
pub fn built_in_project(
    listing: &std::collections::HashSet<String>,
    statuses: &HashMap<String, BuildStatus>,
    dep: &str,
) -> Option<String> {
    let bare = fuzzy_key(dep);
    let rpm = fuzzy_key(&rpm_name(dep));
    listing
        .iter()
        .filter(|p| {
            let k = fuzzy_key(p);
            k == bare || k == rpm
        })
        .find(|p| statuses.get(*p).is_some_and(BuildStatus::is_success))
        .cloned()
}
mod spec_info;
pub use spec_info::*;

#[cfg(test)]
mod sourceinfo_version_tests;

/// Whether `info` (a project meta) builds for `repo` on `arch`: the `Err`
/// says what the project does offer. A meta with no repository at all is
/// left to the "no build repositories" handling elsewhere.
fn check_repo_arch(
    info: &ProjectInfo,
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<(), String> {
    if info.repositories.is_empty() {
        return Ok(());
    }
    let Some(def) = info.repositories.iter().find(|r| r.name == repo) else {
        let names: Vec<&str> = info.repositories.iter().map(|r| r.name.as_str()).collect();
        return Err(format!(
            "{project} has no repository {repo} (it has: {})",
            names.join(", ")
        ));
    };
    if !def.arches.iter().any(|a| a == arch) {
        return Err(format!(
            "{project} does not build {repo} for {arch} (it builds: {})",
            def.arches.join(", ")
        ));
    }
    Ok(())
}

/// The repository of a project that stands for `wanted` on `arch`: `wanted`
/// itself when the project has it, else the one the distribution is also known
/// by (`openSUSE_Tumbleweed` is built as `openSUSE_Factory` in Factory's devel
/// projects, and a branch takes its repositories from the project it was
/// branched from), else the only repository that builds `arch`.
fn pick_repository(info: &ProjectInfo, wanted: &str, arch: &str) -> Result<String, String> {
    let builds = |name: &str| {
        info.repositories
            .iter()
            .find(|r| r.name == name)
            .is_some_and(|r| r.arches.iter().any(|a| a == arch))
    };
    if builds(wanted) {
        return Ok(wanted.to_string());
    }
    let aliases: &[&str] = match wanted {
        "openSUSE_Tumbleweed" => &["openSUSE_Factory"],
        "openSUSE_Factory" => &["openSUSE_Tumbleweed"],
        _ => &[],
    };
    if let Some(alias) = aliases.iter().find(|a| builds(a)) {
        return Ok((*alias).to_string());
    }
    let mut serving = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch));
    match (serving.next(), serving.next()) {
        (Some(only), None) => Ok(only.name.clone()),
        _ => Err(format!(
            "no repository for {wanted} on {arch} (it has: {})",
            info.repositories
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The repository to build for when none was asked for: Tumbleweed, else
/// Factory, else the one repository that builds `arch`, else the first by name.
/// The others that build `arch` come second, so the caller can say what it
/// passed over.
fn default_repository(info: &ProjectInfo, arch: &str) -> Option<(String, Vec<String>)> {
    let mut serving: Vec<&str> = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch))
        .map(|r| r.name.as_str())
        .collect();
    serving.sort_unstable();
    let chosen = ["openSUSE_Tumbleweed", "openSUSE_Factory"]
        .into_iter()
        .find(|p| serving.contains(p))
        .or_else(|| serving.first().copied())?
        .to_string();
    let others = serving
        .into_iter()
        .filter(|n| *n != chosen)
        .map(str::to_string)
        .collect();
    Some((chosen, others))
}

/// The repository of `project` to build for on `arch`: the one asked for, as
/// [`resolve_repository`] reads it, or, when `requested` is `None`, the
/// project's own preferred one. The second part is the other repositories that
/// build `arch` when the choice was between several. A project that does not
/// exist yet, or whose meta cannot be read, gets `openSUSE_Tumbleweed`.
pub fn choose_repository(
    project: &str,
    requested: Option<&str>,
    arch: &str,
) -> (String, Vec<String>) {
    const FALLBACK: &str = "openSUSE_Tumbleweed";
    if let Some(wanted) = requested {
        return (resolve_repository(project, wanted, arch), Vec::new());
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return (FALLBACK.to_string(), Vec::new());
    };
    default_repository(&parse_project_info(&meta), arch)
        .unwrap_or_else(|| (FALLBACK.to_string(), Vec::new()))
}

/// [`pick_repository`] for a project on OBS, remembered for the run. A project
/// whose meta cannot be read keeps `wanted`, so the build's own check reports
/// what is wrong.
pub fn resolve_repository(project: &str, wanted: &str, arch: &str) -> String {
    /// (project, wanted, arch) → the repository chosen.
    type Known = HashMap<(String, String, String), String>;
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let key = (project.to_string(), wanted.to_string(), arch.to_string());
    if let Some(hit) = KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key))
    {
        return hit.clone();
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return wanted.to_string();
    };
    let info = parse_project_info(&meta);
    if info.repositories.is_empty() {
        return wanted.to_string();
    }
    let chosen = pick_repository(&info, wanted, arch).unwrap_or_else(|_| wanted.to_string());
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key, chosen.clone());
    chosen
}

/// Fail before waiting when the project does not build for `repo`/`arch`:
/// OBS would answer with no result at all, forever. A meta that cannot be
/// read skips the check; the wait's own "no results" limit still applies.
fn ensure_builds_for(project: &str, repo: &str, arch: &str) -> Result<(), Error> {
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return Ok(());
    };
    check_repo_arch(&parse_project_info(&meta), project, repo, arch).map_err(Error::NotActionable)
}

/// How long to wait before the next status poll. A status that keeps
/// holding is polled less often (15s, then 30s after 4 unchanged polls, then
/// 60s after 8) and a failing poll backs off exponentially up to 2 minutes, so
/// a long build or an ailing server is not hammered. `jitter` in `[0, 1)`
/// spreads concurrent waiters by up to +-20%.
fn poll_delay(unchanged_polls: u32, errors: u32, jitter: f64) -> Duration {
    let base = match unchanged_polls {
        0..=3 => 15,
        4..=7 => 30,
        _ => 60,
    };
    let secs = if errors == 0 {
        base
    } else {
        (base << errors.min(4)).min(120)
    };
    Duration::from_secs_f64(secs as f64 * (0.8 + 0.4 * jitter))
}

/// The unchanged-poll count that sets the delay while waiting for a build. A
/// status that only awaits the new revision (see
/// [`obs_status::awaiting_new_revision`]) is not "holding still": the build is
/// about to start, so it never backs off past the 30s tier.
fn wait_polls(unchanged_polls: u32, awaiting_new_revision: bool) -> u32 {
    if awaiting_new_revision {
        unchanged_polls.min(7)
    } else {
        unchanged_polls
    }
}

/// How long an unchanged status holds before the independent `osc results`
/// cross-check, asked once per stuck period (see [`recheck_due`]).
const NO_PROGRESS_RECHECK: Duration = Duration::from_secs(300);
// A `stale`/`unconfirmed` status is the previous revision's verdict served
// while OBS has not taken up the commit (or has, and the document has not
// caught up): the independent read says which, so it is asked early and again
// at this pace, instead of once after minutes.
const STALE_RECHECK_FIRST: Duration = Duration::from_secs(30);
const STALE_RECHECK_EVERY: Duration = Duration::from_secs(60);

/// Whether the independent `osc results` cross-check of a non-terminal status
/// is due: `since_change` is how long the status has not changed,
/// `since_recheck` how long ago the last check of this stuck period was. A
/// status that awaits the new revision is checked after 30s and then every
/// 60s; any other unchanged status once, after 300s.
fn recheck_due(
    awaiting_new_revision: bool,
    since_change: Duration,
    since_recheck: Option<Duration>,
) -> bool {
    match (awaiting_new_revision, since_recheck) {
        (true, None) => since_change >= STALE_RECHECK_FIRST,
        (true, Some(last)) => last >= STALE_RECHECK_EVERY,
        (false, None) => since_change >= NO_PROGRESS_RECHECK,
        (false, Some(_)) => false,
    }
}

/// A pseudo-random value in `[0, 1)` from the clock, enough to de-synchronise
/// pollers without a randomness dependency.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

/// The `<details>` OBS gives a package's result in `repo`/`arch` (what a
/// `blocked` or `scheduled` package is waiting for), if any.
fn build_details(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Option<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return None;
    }
    let entries = obs_package_entries(project, repo, arch, pkg_name, None)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()??;
    obs_status::entry_in(&entries, repo, arch, pkg_name)?
        .details
        .clone()
}

/// Sleep for `total`, waking each second to notice a shutdown request, so a
/// long poll delay does not hold up an interrupt.
pub(crate) fn sleep_interruptible(total: Duration) {
    let end = Instant::now() + total;
    while !crate::proc::shutting_down() {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    }
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
fn build_with_retriggers(
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
const BROKEN_MESSAGE: &str = "OBS: package source is broken";
/// Internal marker: `broken` without a build log behind it (see [`obs_build`]).
const BROKEN_UNCONFIRMED: &str = "OBS: broken status could not be confirmed by a build log";
/// How often an unconfirmed `broken` verdict is re-triggered before it stands.
const MAX_BROKEN_RETRIGGERS: u32 = 3;
const BROKEN_RETRIGGER_PAUSE: Duration = Duration::from_secs(30);

/// Whether a `broken` verdict is backed by evidence: a non-empty build log.
fn broken_is_confirmed(log_path: &Path) -> bool {
    read_log_lossy(log_path).is_ok_and(|t| !t.trim().is_empty())
}

/// A build log as text, lossily: a log with one invalid UTF-8 byte is still
/// the evidence it was, and must not read as an empty or missing file.
fn read_log_lossy(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The text of a build log that is the evidence for a failure. One that
/// cannot be read, or is empty (a failed `osc buildlog` leaves an empty file
/// behind), is an environment problem, reported as an I/O error.
fn usable_build_log(log_path: &Path) -> Result<String, Error> {
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
fn retry<T, E>(
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

/// One package's `getbinaries`, `get` injected: retried up to `attempts` times,
/// and each try waits for free disk space first (a full disk fails the try with
/// code 28 without running `get`).
#[allow(clippy::too_many_arguments)]
fn fetch_with_retries(
    project: &str,
    name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
    attempts: u32,
    pause: Duration,
    mut get: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    retry(attempts, pause, || {
        if let Err(stderr) = crate::workspace::await_disk_space(rpms_dir, emit.reporter, emit.step)
        {
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
            rpms_dir,
            emit,
            GETBINARIES_ATTEMPTS,
            GETBINARIES_PAUSE,
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
fn stash_rpmlint_log(rpms_dir: &Path, name: &str) {
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

fn obs_build_once(
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
    // The caller has committed already: no `_result` document fetched before
    // this may be used for the status (see `get_build_status_after_commit`).
    let committed = Instant::now();
    ensure_builds_for(project, repo, arch)?;
    const MAX_WAIT: Duration = Duration::from_secs(4 * 3600);
    // Time spent `blocked` (waiting for other packages to build) does not
    // count against MAX_WAIT while the blocker is visibly moving, i.e. its
    // details changed within BLOCK_STALL; the wait as a whole is still
    // bounded by MAX_TOTAL_WAIT.
    const BLOCK_STALL: Duration = Duration::from_secs(600);
    const MAX_TOTAL_WAIT: Duration = Duration::from_secs(6 * 3600);

    let start = Instant::now();
    let mut prev_status = String::new();

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;
    // Polls in a row that saw the same status; the delay between polls grows
    // with it (see `poll_delay`).
    let mut unchanged_polls: u32 = 0;

    // The previous mechanism only ever broke silence for one specific case
    // (an `Unknown("stale: ...")` verdict). A plain non-terminal status
    // (`building`, `scheduled`, ...) that simply never changes again —
    // because the XML `_result` document this reads is serving a stale
    // snapshot while OBS has actually moved on — produced no `Progress`
    // event (nothing to report: the string didn't change) and no `Warning`
    // either, so the wait looked indistinguishable from a healthy long
    // build for up to the full `MAX_WAIT`. Reported live: the TUI sat on an
    // unchanging "building" for 1000+ seconds while OBS itself showed
    // nothing in progress.
    const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);
    let mut last_change = Instant::now();
    let mut last_warning = Instant::now();
    let mut last_recheck: Option<Instant> = None;
    let mut last_table_label = String::new();
    const UNRESOLVABLE_CONFIRM: Duration = Duration::from_secs(15);
    let mut unresolvable_since: Option<Instant> = None;
    // A repository/arch the project does not build for has no result at all,
    // which would otherwise read as "not started yet" until MAX_WAIT. A
    // freshly committed package usually shows up within seconds, but a busy
    // scheduler can take minutes: after this long the project meta is
    // consulted again, and only a repository/arch it lacks ends the wait.
    const NO_RESULTS_LIMIT: Duration = Duration::from_secs(600);
    let mut no_results_since: Option<Instant> = None;
    // A built verdict whose job history cannot be read at all is accepted
    // after this long (see `obs_status::accept_unconfirmed`).
    const UNCONFIRMED_LIMIT: Duration = Duration::from_secs(300);
    let mut unconfirmed_since: Option<Instant> = None;
    let mut blocked_credit = Duration::ZERO;
    let mut last_tick = Instant::now();
    let mut last_block_change = Instant::now();
    let mut was_blocked = false;

    let status = loop {
        if crate::proc::shutting_down() {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: "interrupted".to_string(),
            });
        }
        let tick = last_tick.elapsed();
        last_tick = Instant::now();
        if was_blocked && last_block_change.elapsed() < BLOCK_STALL {
            blocked_credit += tick;
        }
        let waited = start.elapsed();
        if waited.saturating_sub(blocked_credit) >= MAX_WAIT || waited >= MAX_TOTAL_WAIT {
            // Which limit tripped: the total one only when it did so alone.
            let limit = if waited >= MAX_TOTAL_WAIT {
                MAX_TOTAL_WAIT
            } else {
                MAX_WAIT
            };
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s (limit {}s) waiting for OBS build (last status: {})",
                    waited.as_secs(),
                    limit.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status_after_commit(project, pkg_name, repo, arch, committed) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(Error::OscFailed {
                        cmd: format!("wait {project}/{pkg_name} ({repo}/{arch})"),
                        code: -1,
                        stderr: format!(
                            "gave up after {poll_errors} consecutive failed status polls; last error: {e}"
                        ),
                    });
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                sleep_interruptible(poll_delay(unchanged_polls, poll_errors, jitter()));
                continue;
            }
        };
        if status == BuildStatus::Unknown("no results".to_string()) {
            let since = *no_results_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_RESULTS_LIMIT {
                // The project meta decides: a repository/arch it does not
                // build is a dead end, anything else is a slow scheduler
                // and MAX_WAIT bounds the wait.
                ensure_builds_for(project, repo, arch)?;
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS has reported no build result for {pkg_name} on {repo}/{arch} \
                         for {}s although {project} builds for it; the scheduler is slow, \
                         still waiting",
                        since.elapsed().as_secs()
                    ),
                });
                no_results_since = Some(Instant::now());
            }
        } else {
            no_results_since = None;
        }
        // A verdict no job history could confirm (empty or evicted history,
        // failing API calls) is not worth waiting on forever: settle for it.
        let status = match obs_status::accept_unconfirmed(&status) {
            Some(accepted) => {
                let since = *unconfirmed_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNCONFIRMED_LIMIT {
                    reporter.report(&Event::Warning {
                        step,
                        message: format!(
                            "no OBS job history to confirm the {accepted} result for \
                             {pkg_name} after {}s; accepting it unconfirmed",
                            since.elapsed().as_secs()
                        ),
                    });
                    accepted
                } else {
                    status
                }
            }
            None => {
                unconfirmed_since = None;
                status
            }
        };
        let status_str = status.to_string();
        // What blocks or delays the package is part of the status: a change
        // in it is progress worth showing, and keeps the polling brisk.
        let label = match status {
            BuildStatus::Blocked | BuildStatus::Scheduled => {
                match build_details(project, pkg_name, repo, arch) {
                    Some(d) => format!("{status_str}: {d}"),
                    None => status_str.clone(),
                }
            }
            _ => status_str.clone(),
        };
        was_blocked = status == BuildStatus::Blocked;

        let mut status = status;
        if label == prev_status {
            unchanged_polls = unchanged_polls.saturating_add(1);
        } else {
            unchanged_polls = 0;
        }
        if label != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {label}"),
            });
            if was_blocked && resets_block_timer(&prev_status) {
                last_block_change = Instant::now();
            }
            prev_status = label;
            last_change = Instant::now();
            last_warning = Instant::now();
            last_recheck = None;
        } else if !status.is_terminal() {
            // Non-terminal and unchanged: make sure the user can always
            // tell this is still alive and polling, not silently wedged.
            if last_warning.elapsed() >= NO_PROGRESS_WARNING {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS build status still {prev_status} after {}s with no change",
                        last_change.elapsed().as_secs()
                    ),
                });
                last_warning = Instant::now();
            }
            // An independent cross-check, via a different OBS endpoint than
            // whichever one produced `status` (see
            // `get_build_status_from_results_table`'s doc comment) — catches
            // one of them silently serving a stale snapshot while the other
            // has already moved on. Once per stuck period, or, while the
            // status only says the newest sources are not built yet, soon
            // and then every minute (see `recheck_due`).
            if recheck_due(
                obs_status::awaiting_new_revision(&status),
                last_change.elapsed(),
                last_recheck.map(|t| t.elapsed()),
            ) {
                last_recheck = Some(Instant::now());
                if let Ok(table_status) = get_build_status_from_results_table(
                    project,
                    pkg_name,
                    repo,
                    arch,
                    Emit::quiet(),
                ) {
                    let table_str = table_status.to_string();
                    match obs_status::cross_check(&status, &table_status) {
                        obs_status::CrossCheck::Agrees | obs_status::CrossCheck::Ignore => {}
                        obs_status::CrossCheck::NewRevisionSeen => {
                            // The scheduler works on the new revision: the
                            // wait is progressing, so it polls briskly again.
                            unchanged_polls = 0;
                            if table_str != last_table_label {
                                reporter.report(&Event::Progress {
                                    step,
                                    message: format!(
                                        "OBS has taken up the new revision: {table_str}"
                                    ),
                                });
                                last_table_label = table_str;
                            }
                        }
                        obs_status::CrossCheck::Adopt => {
                            reporter.report(&Event::Warning {
                                step,
                                message: format!(
                                    "OBS status disagreement after {}s of no change: the API \
                                     document says {status_str}, but `osc results` says \
                                     {table_str} — trusting the independent read",
                                    last_change.elapsed().as_secs()
                                ),
                            });
                            reporter.report(&Event::Progress {
                                step,
                                message: format!("OBS build status: {table_str}"),
                            });
                            prev_status = table_str;
                            last_change = Instant::now();
                            last_warning = Instant::now();
                            last_recheck = None;
                            unchanged_polls = 0;
                            status = table_status;
                        }
                    }
                }
            }
        }

        // `unresolvable` is only the scheduler's current view: a dependency
        // published a moment later turns it back into `scheduled`. Accept it
        // as the answer only once a later poll agrees (UNRESOLVABLE_CONFIRM is
        // one poll interval, so a genuine verdict costs a single extra poll).
        if status == BuildStatus::Unresolvable {
            let since = *unresolvable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= UNRESOLVABLE_CONFIRM {
                break status;
            }
        } else {
            unresolvable_since = None;
            if status.is_terminal() {
                break status;
            }
        }

        // `stale: <verdict>` is not a final answer: that terminal result is
        // from a source revision the server may already have superseded, so
        // keep polling. Only a verdict confirmed for the current sources
        // (see `obs_status::resolve`) ends the wait; if none arrives, the
        // MAX_WAIT check above reports a timeout error.
        sleep_interruptible(poll_delay(
            wait_polls(unchanged_polls, obs_status::awaiting_new_revision(&status)),
            0,
            jitter(),
        ));
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    // A missing or stale log leaves the failure without its cause, so say so.
    // Announce the command that really writes `log_path`; the polling above
    // goes through the API and runs no `osc results --watch`.
    //
    // A multibuild package whose own entry built but whose flavor did not has
    // the flavor's log as the evidence: the package's would show a success and
    // send the fix agent after a failure it cannot see.
    let log_pkg = if status.is_success() {
        pkg_name.to_string()
    } else {
        failing_result(project, pkg_name, repo, arch, &status)
    };
    if log_pkg != pkg_name {
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "{pkg_name} itself built, but its multibuild flavor {log_pkg} did not: \
                 the build log is {log_pkg}'s"
            ),
        });
    }
    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "buildlog".to_string(),
            project.to_string(),
            log_pkg.clone(),
            repo.to_string(),
            arch.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });
    if let Err(e) = fetch_buildlog(project, &log_pkg, repo, arch, log_path, Emit::quiet()) {
        reporter.report(&Event::Warning {
            step,
            message: format!("could not fetch the OBS build log for {log_pkg}: {e}"),
        });
    }
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. A succeeded status followed by a failing
        // getbinaries must not still return success with the RPM directory
        // empty, so this call is fatal: a succeeded OBS build without any
        // locally-downloaded artifacts is NOT a success a dependent
        // build can rely on — `rpm_satisfies_dep` can't find anything,
        // the dep is reported unresolvable even though OBS says the
        // source is green, and the dependent spends attempts chasing a
        // nonexistent problem. Promote this to fatal: if OBS says
        // succeeded but we cannot pull the binaries, surface that as a
        // build failure.
        //
        // We also verify that at least one `.rpm` landed under
        // `rpms_dir` after the fetch — `osc getbinaries` can exit 0
        // with no files written when the project is misconfigured
        // (empty binary set for this repo/arch pair), which the warning
        // version silently papered over.
        let fetch_result = download_built_rpms(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = fetch_result.as_ref().copied().unwrap_or(0);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            let advice = if e.is_disk_full() {
                // The build is fine; only this machine could not take the files.
                "The build itself succeeded: free disk space and run again."
            } else {
                "A green source without downloadable artifacts is not a usable \
                 build — a dependent package would see every BuildRequires as \
                 unresolvable. Re-check the OBS project's binary publishing config \
                 for repo={repo}/arch={arch}."
            };
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} {DOWNLOAD_FAILED}: {e}. {}",
                advice.replace("{repo}", repo).replace("{arch}", arch)
            )));
        }
        if downloaded_count == 0 {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded and `osc getbinaries` exited 0, \
                 but no .rpm files appeared under {}. The project's binary \
                 publishing for repo={repo}/arch={arch} is probably misconfigured \
                 (empty binary set) — check the OBS <publish>/<binaries> settings.",
                rpms_dir.display()
            )));
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

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) = current_unresolvable(project, &log_pkg, repo, arch, log_path);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken if !broken_is_confirmed(log_path) => {
            Error::NotActionable(BROKEN_UNCONFIRMED.to_string())
        }
        BuildStatus::Broken => {
            Error::NotActionable(match build_details(project, &log_pkg, repo, arch) {
                Some(d) => format!("{BROKEN_MESSAGE}: {d}"),
                None => BROKEN_MESSAGE.to_string(),
            })
        }
        BuildStatus::Disabled => {
            Error::NotActionable("OBS: build is disabled in project config".to_string())
        }
        BuildStatus::Excluded => {
            Error::NotActionable("OBS: build is excluded in project config".to_string())
        }
        _ => {
            // The log is the only evidence for what went wrong. One that
            // cannot be read, or is empty (a failed `osc buildlog` leaves an
            // empty file behind), is an environment problem: classifying it
            // would send the package to the fix agent as a BuildFailed.
            let log_text = usable_build_log(log_path)?;
            match classify_build_failure(&log_text) {
                FailureKind::NoRepositories => Error::NotActionable(
                    "OBS project has no build repositories — configure them in OBS first"
                        .to_string(),
                ),
                FailureKind::NotWorkingCopy => {
                    Error::NotActionable("osc checkout is not server-backed".to_string())
                }
                FailureKind::UnresolvableDeps { names, detail } => {
                    Error::UnresolvableDeps { names, detail }
                }
                FailureKind::BuildFailed => Error::BuildFailed {
                    spec: package_spec_name(project, pkg_name, emit),
                    code: 1,
                    log: log_path.to_owned(),
                },
            }
        }
    })
}

#[cfg(test)]
mod package_listing_tests;

#[cfg(test)]
mod obs_name_tests;

#[cfg(test)]
mod listing_failure_tests;

#[cfg(test)]
mod count_rpms_tests;

#[cfg(test)]
mod project_build_statuses_tests;

#[cfg(test)]
mod project_repo_paths_tests;

#[cfg(test)]
mod rpmspec_sandbox_tests;

#[cfg(test)]
mod publish_plan_tests;

#[cfg(test)]
mod service_validation_tests;

#[cfg(test)]
mod dash_name_tests;

#[cfg(test)]
mod repo_arch_tests;

#[cfg(test)]
mod poll_tests;

#[cfg(test)]
mod overall_status_tests;

#[cfg(test)]
mod superseded_rpm_tests;

#[cfg(test)]
mod redist_name_tests;

#[cfg(test)]
mod transient_broken_tests;

#[cfg(test)]
mod transient_error_tests;
