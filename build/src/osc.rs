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

/// Query the OBS build status for one repo/arch combination.
///
/// Reads `/build/<project>/_result` (XML) and interprets it with
/// [`obs_status::resolve`]: a bare `finished` is resolved through its
/// `<details>` verdict, and a terminal result in a `dirty` repository is only
/// trusted once the job history confirms it was built from the source
/// revision currently on the server (otherwise `Unknown("stale: ...")`).
///
/// If the API call cannot run or its answer lacks the package, the status is
/// `Unknown` for this read, so the build loop polls again instead of failing.
/// An HTTP error from the API (401, 429, 503, ...) is returned as is.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Result<BuildStatus, Error> {
    match get_build_status_xml(project, pkg_name, repo, arch, false, None)? {
        Some(status) => Ok(status),
        None => Ok(no_result_status()),
    }
}

/// The status of a package the `_result` document does not list (or could not
/// be read for).
fn no_result_status() -> BuildStatus {
    BuildStatus::Unknown("no results".to_string())
}

/// [`get_build_status`] for a package whose sources were just committed: a
/// built verdict only counts once the job history shows it came from the
/// revision now on the server (see [`obs_status::Evidence::confirm`]), so the
/// previous revision's `failed`/`succeeded` is never mistaken for the result.
/// Without the XML answer there is no job history to check, so the status is
/// `Unknown`. `committed` is when the commit was done: the `_result` document
/// is never older than that (see [`ResultsCache::get`]).
fn get_build_status_after_commit(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    committed: Instant,
) -> Result<BuildStatus, Error> {
    match get_build_status_xml(project, pkg_name, repo, arch, true, Some(committed))? {
        Some(status) => Ok(status),
        None => Ok(no_result_status()),
    }
}

/// What every repository/arch of `pkg_name` says, as one status: `Succeeded`
/// only when each result of the package (all repositories and architectures,
/// multibuild flavors included) is succeeded, excluded or disabled; otherwise
/// the first result that is not, with the `repo/arch` it is from. For
/// deciding whether the package may be submitted, where a build that is green
/// on `repo`/`arch` but failed on another architecture is not a good update.
/// `repo`/`arch` is read the careful way ([`get_build_status`]); the others
/// by their plain verdict, polled for a bounded time while any is still
/// scheduled, blocked or building.
pub fn get_overall_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Result<(BuildStatus, Option<String>), Error> {
    const MAX_WAIT: Duration = Duration::from_secs(1800);
    let own = get_build_status(project, pkg_name, repo, arch)?;
    if !own.is_success() {
        return Ok((own, Some(format!("{repo}/{arch}"))));
    }
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    let path = format!(
        "/build/{}/_result?package={}&multibuild=1",
        enc(project),
        enc(pkg_name)
    );
    let start = Instant::now();
    let mut unchanged: u32 = 0;
    let mut last: Option<(BuildStatus, Option<String>)> = None;
    loop {
        let xml = osc_api_checked(&path).map_err(|e| match e {
            ApiFailure::Http(code, msg) => Error::OscFailed {
                cmd: format!("api {path}"),
                code: i32::from(code),
                stderr: msg,
            },
            ApiFailure::Other(msg) => Error::NotActionable(msg),
        })?;
        let entries =
            obs_status::try_parse_results(&xml).map_err(|e| Error::NotActionable(e.to_string()))?;
        let overall = overall_status(&entries, pkg_name);
        let active = matches!(
            overall.0,
            BuildStatus::Scheduled | BuildStatus::Blocked | BuildStatus::Building
        ) || overall.0 == BuildStatus::Unknown("finished".to_string());
        if !active || start.elapsed() >= MAX_WAIT || crate::proc::shutting_down() {
            return Ok(overall);
        }
        unchanged = if last.as_ref() == Some(&overall) {
            unchanged.saturating_add(1)
        } else {
            0
        };
        last = Some(overall);
        sleep_interruptible(poll_delay(unchanged, 0, jitter()));
    }
}

/// The aggregate of [`get_overall_build_status`], from a `_result` document's
/// entries: only the package and its `<package>:<flavor>` multibuild entries
/// count. A result that failed outranks one still going, since waiting on the
/// latter cannot change the outcome. Excluded and disabled results are out of
/// scope; the `repo/arch` of the blocking result comes with the status.
fn overall_status(
    entries: &[obs_status::ResultEntry],
    pkg_name: &str,
) -> (BuildStatus, Option<String>) {
    let flavor_prefix = format!("{pkg_name}:");
    let statuses: Vec<(BuildStatus, String)> = entries
        .iter()
        .filter(|e| e.pkg == pkg_name || e.pkg.starts_with(&flavor_prefix))
        .map(|e| {
            (
                obs_status::resolve_lenient(e),
                format!("{}/{}", e.repo, e.arch),
            )
        })
        .collect();
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    if let Some((bad, at)) = statuses
        .iter()
        .find(|(s, _)| blocks(s) && s.is_terminal())
        .or_else(|| statuses.iter().find(|(s, _)| blocks(s)))
    {
        return (bad.clone(), Some(at.clone()));
    }
    if statuses.iter().any(|(s, _)| s.is_success()) {
        (BuildStatus::Succeeded, None)
    } else {
        (BuildStatus::Unknown("no result built".to_string()), None)
    }
}

/// The `osc results` human-table path only — no XML, no staleness resolution
/// via job history. Used by [`obs_build`] as an independent second opinion
/// when the XML path's answer has stopped changing for a long time: the two
/// paths hit different OBS code (an XML API document vs. a
/// human-table-rendering endpoint), so if one of them is silently serving a
/// stale/cached snapshot, the other is the cheapest way to notice without
/// guessing at why.
fn get_build_status_from_results_table(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let cmd = format!("results --verbose -r {repo} -a {arch} {project} {pkg_name}");
    // `--verbose` is what carries a `finished` result's verdict.
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_single_build_status(&out.stdout, pkg_name, repo, arch))
}

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for `osc co`, `up` and `ci`, which move a package's sources.
const OSC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Bound for `osc buildlog` and `getbinaries`, which move whole logs and RPM
/// sets and so run longer than a source transfer.
const OSC_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    proc::capture_with_timeout("osc", args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
        .map_err(|e| ap_core::diag!("  osc {what}: {e}"))
        .ok()
}

/// Why an `osc api` read gave no document.
#[derive(Clone)]
enum ApiFailure {
    /// The server answered with this HTTP status (401, 429, 503, ...): asking
    /// again through another `osc` command will not get a different answer.
    Http(u16, String),
    /// `osc` could not run, timed out, or failed without an HTTP status.
    Other(String),
}

/// The HTTP status in an `osc` error message, in either wording `osc`
/// versions use (`HTTP Error 503: ...`, `503 Server Error: ...`).
fn http_status(stderr: &str) -> Option<u16> {
    let digits = |s: &str| {
        let n: String = s.chars().take_while(char::is_ascii_digit).collect();
        (n.len() == 3).then(|| n.parse().ok()).flatten()
    };
    if let Some((_, rest)) = stderr.split_once("HTTP Error ") {
        return digits(rest);
    }
    stderr
        .split_whitespace()
        .zip(stderr.split_whitespace().skip(1))
        .find_map(|(code, kind)| {
            matches!(kind, "Client" | "Server")
                .then(|| digits(code))
                .flatten()
        })
}

/// `osc api <path>` stdout, or why there is none.
fn osc_api_checked(path: &str) -> Result<String, ApiFailure> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
    .map_err(|e| ApiFailure::Other(format!("osc api {path}: {e}")))?;
    if !out.success() {
        let msg = format!("osc api {path}: exit {}: {}", out.code, out.stderr.trim());
        return Err(match http_status(&out.stderr) {
            Some(code) => ApiFailure::Http(code, msg),
            None => ApiFailure::Other(msg),
        });
    }
    Ok(out.stdout)
}

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

/// `osc api <path>` stdout, or `None` on any failure or timeout.
fn osc_api(path: &str) -> Option<String> {
    osc_api_checked(path)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}

/// How long a project-wide `_result` document is shared between waiters.
const RESULTS_TTL: Duration = Duration::from_secs(5);

/// How long a failed fetch is served to the waiters that ask right after it,
/// so a refusing server is asked once, not once per waiter.
const RESULTS_ERROR_TTL: Duration = Duration::from_secs(1);

/// The longest a waiter sits behind another waiter's fetch before fetching
/// itself (a little over [`OSC_QUERY_TIMEOUT`]).
const RESULTS_FLIGHT_WAIT: Duration = Duration::from_secs(70);

type ResultsKey = (String, String, String);

enum Slot {
    /// A fetch is running; `id` tells it apart from a later takeover.
    Fetching { id: u64 },
    /// The outcome of a fetch that began at `started` and ended at `at`.
    Ready {
        started: Instant,
        at: Instant,
        result: Result<Arc<str>, ApiFailure>,
    },
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<ResultsKey, Slot>,
    next_flight: u64,
    /// Bumped per project by [`ResultsCache::invalidate`]: a fetch that began
    /// under an older epoch is not stored.
    epochs: HashMap<String, u64>,
}

/// The project-wide `_result` documents, one per (project, repository, arch),
/// shared by every waiter of the process. Every OBS call is an `osc`
/// subprocess, so tens of waiters polling their own package would start tens
/// of Python interpreters per interval; here they trigger one fetch per
/// [`RESULTS_TTL`] and read their package out of the same document.
struct ResultsCache {
    state: Mutex<CacheState>,
    done: std::sync::Condvar,
}

/// Clears a flight's `Fetching` slot if the fetch unwinds, so the waiters
/// behind it neither hang nor wait for a result that will never come.
struct Flight<'a> {
    cache: &'a ResultsCache,
    key: &'a ResultsKey,
    id: u64,
    landed: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if self.landed {
            return;
        }
        let mut st = self.cache.lock();
        if matches!(st.slots.get(self.key), Some(Slot::Fetching { id, .. }) if *id == self.id) {
            st.slots.remove(self.key);
        }
        drop(st);
        self.cache.done.notify_all();
    }
}

impl ResultsCache {
    fn new() -> Self {
        ResultsCache {
            state: Mutex::new(CacheState::default()),
            done: std::sync::Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The document for `key`, from `fetch` unless a shared one is at most
    /// `ttl` old. Waiters for a key whose fetch is running wait for it instead
    /// of starting another. `newer_than` demands a document whose fetch began
    /// at or after that instant (a waiter that has just committed cannot use
    /// what was asked before): an older one is not served, and an older fetch
    /// in flight is waited out and then replaced.
    fn get(
        &self,
        key: &ResultsKey,
        ttl: Duration,
        newer_than: Option<Instant>,
        fetch: impl FnOnce() -> Result<String, ApiFailure>,
    ) -> Result<Arc<str>, ApiFailure> {
        let deadline = Instant::now() + RESULTS_FLIGHT_WAIT;
        let mut st = self.lock();
        let (id, started, epoch) = loop {
            let fresh_enough = |started: &Instant| newer_than.is_none_or(|t| *started >= t);
            match st.slots.get(key) {
                Some(Slot::Ready {
                    started,
                    at,
                    result,
                }) if fresh_enough(started)
                    && at.elapsed()
                        < if result.is_ok() {
                            ttl
                        } else {
                            ttl.min(RESULTS_ERROR_TTL)
                        } =>
                {
                    return result.clone();
                }
                Some(Slot::Fetching { .. }) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !left.is_zero() {
                        st = self
                            .done
                            .wait_timeout(st, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                }
                _ => {}
            }
            let id = st.next_flight;
            st.next_flight += 1;
            let started = Instant::now();
            st.slots.insert(key.clone(), Slot::Fetching { id });
            break (id, started, st.epochs.get(&key.0).copied().unwrap_or(0));
        };
        drop(st);

        let mut flight = Flight {
            cache: self,
            key,
            id,
            landed: false,
        };
        let result = fetch().map(|doc| Arc::from(doc.as_str()));
        let mut st = self.lock();
        if matches!(st.slots.get(key), Some(Slot::Fetching { id: i, .. }) if *i == id) {
            if st.epochs.get(&key.0).copied().unwrap_or(0) == epoch {
                st.slots.insert(
                    key.clone(),
                    Slot::Ready {
                        started,
                        at: Instant::now(),
                        result: result.clone(),
                    },
                );
            } else {
                st.slots.remove(key);
            }
        }
        flight.landed = true;
        drop(st);
        self.done.notify_all();
        result
    }

    /// Forget `project`'s documents: its sources changed, so what was fetched
    /// so far describes the old state. A fetch in flight is not stored.
    fn invalidate(&self, project: &str) {
        let mut st = self.lock();
        *st.epochs.entry(project.to_string()).or_insert(0) += 1;
        st.slots
            .retain(|k, slot| k.0 != project || matches!(slot, Slot::Fetching { .. }));
    }
}

static RESULTS_CACHE: LazyLock<ResultsCache> = LazyLock::new(ResultsCache::new);

/// Every entry of the `_result` document for `pkg`'s repository/arch, when the
/// document has `pkg` in it. The project-wide document comes from `cache`;
/// one that is cut off, or lacks the package (not scheduled yet, or fetched
/// before it was added), is replaced by `fetch_package`, the document of the
/// package alone (with its multibuild flavors). A failure of the shared fetch
/// is returned as is: asking again another way meets the same refusal.
#[allow(clippy::too_many_arguments)]
fn package_entries(
    cache: &ResultsCache,
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
    fetch_project: impl FnOnce() -> Result<String, ApiFailure>,
    fetch_package: impl FnOnce() -> Result<String, ApiFailure>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    let key = (project.to_string(), repo.to_string(), arch.to_string());
    let doc = cache.get(&key, RESULTS_TTL, fresh_since, fetch_project)?;
    if let Ok(entries) = obs_status::try_parse_results(&doc)
        && obs_status::entry_in(&entries, repo, arch, pkg).is_some()
    {
        return Ok(Some(entries));
    }
    let xml = fetch_package()?;
    let entries = obs_status::parse_results(&xml);
    let found = obs_status::entry_in(&entries, repo, arch, pkg).is_some();
    Ok(found.then_some(entries))
}

/// [`package_entries`] against OBS. The names must be valid OBS names.
fn obs_package_entries(
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    // `multibuild=1` adds the package's `<pkg>:<flavor>` entries to the
    // single-package document; the project-wide one lists them anyway.
    let (project_path, package_path) = (
        format!(
            "/build/{}/_result?repository={}&arch={}",
            enc(project),
            enc(repo),
            enc(arch)
        ),
        format!(
            "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
            enc(project),
            enc(pkg),
            enc(repo),
            enc(arch)
        ),
    );
    package_entries(
        &RESULTS_CACHE,
        project,
        repo,
        arch,
        pkg,
        fresh_since,
        || osc_api_checked(&project_path),
        || osc_api_checked(&package_path),
    )
}

/// `fresh_since`: only a document fetched after that instant may be used
/// (see [`ResultsCache::get`]); `None` accepts one a few seconds old.
fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    confirm: bool,
    fresh_since: Option<Instant>,
) -> Result<Option<BuildStatus>, Error> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Ok(None);
    }
    let entries = match obs_package_entries(project, repo, arch, pkg_name, fresh_since) {
        Ok(Some(entries)) => entries,
        Ok(None) => return Ok(None),
        // An HTTP error (login, rate limit, outage) would repeat on the next
        // read, so it is reported rather than turned into an empty answer.
        Err(ApiFailure::Http(code, msg)) => {
            return Err(Error::OscFailed {
                cmd: format!("api /build/{project}/_result"),
                code: i32::from(code),
                stderr: msg,
            });
        }
        Err(ApiFailure::Other(msg)) => {
            ap_core::diag!("  {msg}");
            return Ok(None);
        }
    };
    let Some(entry) = obs_status::entry_in(&entries, repo, arch, pkg_name) else {
        return Ok(None);
    };
    // The source revision is the package's; every flavor builds from it.
    let mut source: Option<Option<obs_status::SourceInfo>> = None;
    let mut resolve_entry = |entry: &obs_status::ResultEntry| {
        let evidence = if obs_status::needs_evidence(entry, confirm) {
            obs_status::Evidence {
                confirm,
                source: source
                    .get_or_insert_with(|| {
                        osc_api(&format!(
                            "/source/{}/{}?view=info",
                            enc(project),
                            enc(pkg_name)
                        ))
                        .and_then(|x| obs_status::parse_source_info(&x))
                    })
                    .clone(),
                job: osc_api(&format!(
                    "/build/{}/{}/{}/_jobhistory?package={}&limit=1",
                    enc(project),
                    enc(repo),
                    enc(arch),
                    enc(&entry.pkg)
                ))
                .and_then(|x| obs_status::parse_latest_job(&x)),
            }
        } else {
            obs_status::Evidence {
                confirm,
                ..Default::default()
            }
        };
        obs_status::resolve(entry, &evidence)
    };
    let base = resolve_entry(entry);
    let flavors: Vec<BuildStatus> = obs_status::flavors_in(&entries, repo, arch, pkg_name)
        .iter()
        .map(&mut resolve_entry)
        .collect();
    Ok(Some(obs_status::combine(base, &flavors)))
}

#[cfg(test)]
mod results_cache_tests;

#[cfg(test)]
mod wait_pacing_tests;

/// Parsing half of the table read in [`get_build_status_from_results_table`],
/// split out so it is testable without shelling out to `osc`.
///
/// `osc results --verbose -r <repo> -a <arch> <project> <pkg>` prints one line
/// per result as `<repo> <arch> <pkg> <status>` (osc's `get_results` format,
/// columns in that order). The status is the rest of the line:
/// - a plain code (`succeeded`, `failed`, `scheduled`, ...), optionally with
///   `: <details>` after it, and `(unpublished)` for a build that succeeded
///   but is not published yet;
/// - `finished: <verdict>`: OBS hides the verdict behind the bare `finished`
///   code, and `--verbose` prints it as the details;
/// - `outdated (was: ...)` when the repository is dirty, or a trailing `*` in
///   the non-verbose form: the result may be for an older source revision.
///
/// Multi-line `unresolvable:` details only add lines that do not start with
/// the repo, so reading the first matching line is enough.
fn parse_single_build_status(stdout: &str, pkg_name: &str, repo: &str, arch: &str) -> BuildStatus {
    for line in stdout.lines() {
        let mut rest = line.trim_start();
        let mut columns = [""; 3];
        for col in &mut columns {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            *col = &rest[..end];
            rest = rest[end..].trim_start();
        }
        if columns != [repo, arch, pkg_name] {
            continue;
        }
        return status_from_osc_text(rest.trim());
    }
    BuildStatus::Unknown("no results".to_string())
}

/// The bare status word of a result head: the trailing `*` and `(unpublished)`
/// markers removed, with whitespace before each one removed too, so that
/// `succeeded (unpublished)` gives `succeeded`.
fn strip_result_suffixes(head: &str) -> &str {
    let mut word = head;
    loop {
        let next = word
            .trim_end_matches('*')
            .trim_end_matches("(unpublished)")
            .trim_end();
        if next.len() == word.len() {
            return word;
        }
        word = next;
    }
}

/// The status part of one `osc results` line (see [`parse_single_build_status`]).
fn status_from_osc_text(text: &str) -> BuildStatus {
    // OBS marks a result as possibly older than the sources on the server:
    // the dirty-repository form, which `--verbose` spells `outdated (was: ..)`
    // and the plain form spells with a trailing `*`. Never trust those as-is:
    // reporting the stripped word as plain `Succeeded` would make the
    // `AlreadyBuilt` fast path in pipeline.rs believe a since-superseded
    // result still verifies the current revision. `Unknown` makes that path
    // fall through and resync instead.
    if text.starts_with("outdated") {
        return BuildStatus::Unknown(format!("stale: {text}"));
    }
    let (head, details) = match text.split_once(':') {
        Some((head, details)) => (head.trim(), Some(details.trim())),
        None => (text, None),
    };
    let starred = head.ends_with('*');
    let word = strip_result_suffixes(head);
    if starred {
        return BuildStatus::Unknown(format!("stale: {word}"));
    }
    if word == "finished" {
        // The verdict is the first word of the details, if osc printed it.
        return match details.and_then(|d| d.split_whitespace().next()) {
            Some(verdict) => BuildStatus::from(verdict),
            None => BuildStatus::Unknown("finished".to_string()),
        };
    }
    BuildStatus::from(word)
}

#[cfg(test)]
mod single_build_status_tests;

/// Every package's status in `project` for one repository and architecture,
/// read from the `_result` document. Multibuild flavors are folded into their
/// base package (see [`fold_flavor_statuses`]).
pub fn get_project_build_statuses(
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<HashMap<String, BuildStatus>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let path = format!(
        "/build/{}/_result?repository={}&arch={}",
        enc(project),
        enc(repo),
        enc(arch)
    );
    // An empty map reads as "no package has a status", so a listing that
    // cannot be read is an error rather than an empty answer.
    let xml = osc_api_checked(&path).map_err(|e| match e {
        ApiFailure::Http(code, msg) => Error::OscFailed {
            cmd: format!("api {path}"),
            code: i32::from(code),
            stderr: msg,
        },
        ApiFailure::Other(msg) => {
            Error::NotActionable(format!("cannot read the OBS results listing: {msg}"))
        }
    })?;
    // A cut-off listing must not read as a partial set of statuses.
    let statuses = project_statuses_from_xml(&xml, repo, arch)
        .map_err(|e| Error::NotActionable(format!("cannot read the OBS results listing: {e}")))?;
    Ok(fold_flavor_statuses(statuses))
}

/// A package is only as built as its worst multibuild flavor: `python-mcp` is
/// `succeeded` while its `python-mcp:test` flavor is `unresolvable` (a test
/// dependency nothing provides), and counting the base alone as built left the
/// package out of the build queue with its missing dependency never reported.
/// A flavor that is not `succeeded`/`excluded`/`disabled` passes its status to
/// the base package when the base would otherwise count as built. The flavor
/// entries stay in the map.
fn fold_flavor_statuses(
    mut statuses: HashMap<String, BuildStatus>,
) -> HashMap<String, BuildStatus> {
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    let flavors: Vec<(String, BuildStatus)> = statuses
        .iter()
        .filter(|(k, s)| k.contains(':') && blocks(s))
        // A `test` flavor whose tests FAIL builds nothing a dependent installs,
        // and the package it belongs to cannot be fixed by building it again:
        // python-redis (`python-redis:test` failed, `python-redis` succeeded) was
        // selected as unbuilt every round, finished "already built" every
        // round, and the requested root was never reached. Only a test flavor
        // that cannot even be set up (`unresolvable`, `broken`) still counts.
        .filter(|(k, s)| !(k.ends_with(":test") && matches!(s, BuildStatus::Failed)))
        .map(|(k, s)| (k.clone(), s.clone()))
        .collect();
    for (key, status) in flavors {
        let Some((base, _)) = key.split_once(':') else {
            continue;
        };
        if let Some(b) = statuses.get_mut(base)
            && b.is_success()
        {
            *b = status;
        }
    }
    statuses
}

/// Every package's status in ONE repository/architecture of a `_result`
/// document.
///
/// The document carries a `<result>` per repository/architecture the project
/// builds for, and the same package is routinely `succeeded` in one and
/// `excluded` or `unresolvable` in another (an `i586` repository next to
/// `x86_64`); only the pair asked for may contribute, or one package's state
/// somewhere else would stand in for the state of the build being made.
fn project_statuses_from_xml(
    xml: &str,
    repo: &str,
    arch: &str,
) -> Result<HashMap<String, BuildStatus>, String> {
    Ok(obs_status::try_parse_results(xml)?
        .into_iter()
        .filter(|e| e.repo == repo && e.arch == arch)
        .map(|e| (e.pkg.clone(), obs_status::resolve_lenient(&e)))
        .collect())
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
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "buildlog {project} {pkg_name} {repo} {arch}"
    )))?;

    proc::write_log(log_path, out.stdout.as_bytes())?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ask OBS to build `pkg_name` again from the sources it already has, and wait
/// (at most 90 s) until its status is no longer a final one. Without that wait
/// the build wait that follows would read the previous build's verdict, which
/// matches the current sources, as the answer. Returns whether the new build
/// was seen to start.
pub fn rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
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
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) && !crate::proc::shutting_down() {
        if get_build_status(project, pkg_name, repo, arch).is_ok_and(|s| !s.is_terminal()) {
            return Ok(true);
        }
        sleep_interruptible(Duration::from_secs(5));
    }
    Ok(false)
}

/// Download built binaries from OBS into `dest_dir`.
///
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting the RPMs of `pkg` written since `since` (the start of
/// this package's download), see [`count_package_rpms_since`].
fn downloaded_rpm_count(
    listing: Option<&[String]>,
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_package_rpms_since_in_dir(rpms_dir, pkg, since),
    }
}

/// The `.rpm` binaries OBS lists for `pkg_name` in `repo`/`arch`, or `None`
/// when the listing cannot be read.
fn binary_rpm_listing(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<Vec<String>> {
    osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml))
}

/// The package name of an RPM file name: `python313-foo-1.0-2.1.noarch.rpm`
/// is `python313-foo`.
fn rpm_base_name(file: &str) -> Option<&str> {
    let (rest, _arch) = file.strip_suffix(".rpm")?.rsplit_once('.')?;
    let (rest, _release) = rest.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    Some(name)
}

/// Delete the RPMs in `rpms_dir` that an earlier build of the same package
/// left behind: files named like one of `current` (same package name),
/// including a `current` file itself, so that a download which wrote nothing
/// is not mistaken for one that did by a stale file of the same name.
/// `rpms_dir` is shared by every package, so they would otherwise pile up and
/// satisfy a dependency with an old version.
fn remove_superseded_rpms(rpms_dir: &Path, current: &[String]) {
    let names: std::collections::HashSet<&str> =
        current.iter().filter_map(|f| rpm_base_name(f)).collect();
    let Ok(entries) = fs::read_dir(rpms_dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.ends_with(".src.rpm") {
            continue;
        }
        if rpm_base_name(&file).is_some_and(|n| names.contains(n))
            && let Err(e) = fs::remove_file(entry.path())
        {
            ap_core::diag!("  cannot remove the stale {file}: {e}");
        }
    }
}

/// The result a non-successful `status` of `pkg_name` comes from: the package
/// itself, or the `<pkg>:<flavor>` multibuild entry that failed while the
/// package built (`python-mcp:test`'s `%check`). The status of a multibuild
/// package is the worst of its entries, so the log that explains it is that
/// entry's, not the package's own, which can be a clean success.
fn failing_result(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    status: &BuildStatus,
) -> String {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return pkg_name.to_string();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .and_then(|xml| failing_entry_name(&xml, repo, arch, pkg_name, status))
    .unwrap_or_else(|| pkg_name.to_string())
}

/// [`failing_result`] over a `_result` document: the package if its own entry
/// has `status`, else the first flavor entry that does.
fn failing_entry_name(
    xml: &str,
    repo: &str,
    arch: &str,
    pkg_name: &str,
    status: &BuildStatus,
) -> Option<String> {
    let base = obs_status::find_entry(xml, repo, arch, pkg_name);
    std::iter::once(base)
        .flatten()
        .chain(obs_status::flavor_entries(xml, repo, arch, pkg_name))
        .find(|e| obs_status::resolve_lenient(e) == *status)
        .map(|e| e.pkg)
        .filter(|name| is_valid_obs_name(name))
}

/// The `<pkg>:<flavor>` names of `pkg_name`'s multibuild flavors built for
/// `repo`/`arch`; empty for a plain package or when the listing cannot be read.
fn multibuild_flavors(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Vec::new();
    }
    osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .map(|xml| {
        obs_status::flavor_entries(&xml, repo, arch, pkg_name)
            .into_iter()
            .filter(|e| {
                is_valid_obs_name(&e.pkg)
                    && !matches!(
                        obs_status::resolve_lenient(e),
                        BuildStatus::Excluded | BuildStatus::Disabled
                    )
            })
            .map(|e| e.pkg)
            .collect()
    })
    .unwrap_or_default()
}

/// Whether the file name `file` is a binary RPM of package `pkg`: `<pkg>-` and
/// then `<version>-<release>.<arch>.rpm`, the same shape `sibling_rpms` takes
/// (a version starting with a digit, exactly two `-` fields after the name).
/// A sibling whose name extends `pkg` (`pkg-libs-1.0-1.x86_64.rpm`) does not
/// match, and neither does a `.src.rpm`.
fn is_package_rpm(file: &str, pkg: &str) -> bool {
    if file.ends_with(".src.rpm") {
        return false;
    }
    let Some(rest) = file.strip_prefix(pkg).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".rpm") else {
        return false;
    };
    match stem.split('-').collect::<Vec<_>>().as_slice() {
        [version, release_arch] => {
            version.starts_with(|c: char| c.is_ascii_digit())
                && release_arch
                    .rsplit_once('.')
                    .is_some_and(|(release, arch)| !release.is_empty() && !arch.is_empty())
        }
        _ => false,
    }
}

/// How many of `files` (name, modification time; `None` when unknown) are
/// binary RPMs of `pkg` modified at or after `since`. Pure: the caller reads
/// the directory.
fn count_package_rpms_since(
    files: &[(String, Option<SystemTime>)],
    pkg: &str,
    since: SystemTime,
) -> usize {
    files
        .iter()
        .filter(|(name, modified)| {
            is_package_rpm(name, pkg) && modified.is_some_and(|m| m >= since)
        })
        .count()
}

/// [`count_package_rpms_since`] over the files currently in `rpms_dir`.
/// Returns 0 when the directory doesn't exist or can't be read.
fn count_package_rpms_since_in_dir(
    rpms_dir: &std::path::Path,
    pkg: &str,
    since: SystemTime,
) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    let files: Vec<(String, Option<SystemTime>)> = iter
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.metadata().and_then(|m| m.modified()).ok(),
            )
        })
        .collect();
    count_package_rpms_since(&files, pkg, since)
}

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
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture_with_timeout(
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
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "getbinaries {project} {pkg_name} {repo} {arch}"
    )))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: the `_result`
/// document first, then whatever build log already sits at `log_path`. Exposed
/// so a caller that finds a package already `Unresolvable` on restart can
/// inform the fix agent immediately instead of spending an attempt
/// rediscovering it through a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
) -> (Vec<String>, Vec<String>) {
    // The `_result` document is what OBS says about the current revision in
    // exactly this repository and architecture; a build log may be left over
    // from an older revision, so it only runs when the document gives nothing.
    if let Some(details) = unresolvable_details(project, pkg_name, repo, arch) {
        let names = extract_unresolvable(&details);
        if !names.is_empty() {
            return (names, extract_unresolvable_detail(&details));
        }
    }
    let log_text = read_log_lossy(log_path).unwrap_or_default();
    (
        extract_unresolvable(&log_text),
        extract_unresolvable_detail(&log_text),
    )
}

/// The `<details>` text of `pkg_name`'s `unresolvable` status in this one
/// repository/architecture, or `None` when it is not unresolvable there (or
/// the status cannot be read). Other repositories' results in the same
/// document are never looked at: a package can be `unresolvable` on `i586`
/// and `succeeded` on `x86_64` at once.
pub fn unresolvable_details(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let xml = osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    unresolvable_details_from_xml(&xml, repo, arch, pkg_name)
}

fn unresolvable_details_from_xml(xml: &str, repo: &str, arch: &str, pkg: &str) -> Option<String> {
    let entry = obs_status::find_entry(xml, repo, arch, pkg)?;
    (BuildStatus::from(entry.code.as_str()) == BuildStatus::Unresolvable)
        .then_some(entry.details)
        .flatten()
}

/// The packages that must be built before the unresolvable `pkg_name` can
/// build, read from OBS's own `nothing provides` details for this
/// repository/architecture and mapped back to package names by
/// [`missing_package_names`]. Empty when OBS names none (not unresolvable
/// here, a version conflict, or an unreadable status).
pub fn unresolvable_blockers(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    match unresolvable_details(project, pkg_name, repo, arch) {
        Some(details) => blockers_from_details(&details),
        None => Vec::new(),
    }
}

fn blockers_from_details(details: &str) -> Vec<String> {
    // A requirement with a version bound is the spec's own constraint failing
    // against the one version the repository has, which building the named
    // package cannot fix; only unconstrained ones are packages to build.
    let unconstrained: Vec<String> = details
        .lines()
        .flat_map(nothing_provides_segments)
        .filter(|segment| segment.split_whitespace().nth(1).is_none())
        .map(|segment| format!("nothing provides {segment}"))
        .collect();
    missing_package_names(&extract_unresolvable(&unconstrained.join("\n")))
}

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

/// Section headers of an RPM spec, for [`spec_section`].
const SPEC_SECTIONS: &[&str] = &[
    "%package",
    "%description",
    "%prep",
    "%conf",
    "%generate_buildrequires",
    "%build",
    "%install",
    "%check",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%files",
    "%changelog",
];

/// The lines of every `name` section (`%files`, `%install`, ...) of `spec`,
/// header lines excluded.
fn spec_section<'a>(spec: &'a str, name: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in spec.lines() {
        let word = line.split_whitespace().next().unwrap_or("");
        if SPEC_SECTIONS.contains(&word) {
            inside = word == name;
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    lines
}

/// Whether `spec` is an unfinished skeleton whose package builds but ships
/// next to nothing, and why: a `%files` that lists only `%license`/`%doc`
/// (nothing the build installed is packaged), or an `%install` that is empty
/// apart from comments next to a `TODO`. Both are what the skeleton
/// generators leave for a monorepo or a wheel they could not lay out, and OBS
/// builds either one "successfully".
pub fn unfinished_spec_reason(spec: &str) -> Option<String> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_code = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let files: Vec<&str> = spec_section(body, "%files")
        .into_iter()
        .filter(is_code)
        .collect();
    let files_listed = files.iter().any(|l| {
        let t = l.trim_start();
        !["%license", "%doc", "%defattr", "%dir"]
            .iter()
            .any(|p| t.starts_with(p))
    });
    let mut reasons = Vec::new();
    if body.contains("\n%files") && !files_listed {
        reasons.push("its %files lists nothing but %license/%doc".to_string());
    }
    if !spec_section(body, "%install").iter().any(is_code)
        && body
            .lines()
            .any(|l| l.trim_start().starts_with('#') && l.contains("TODO"))
    {
        reasons.push("its %install has no commands, only TODO comments".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join(" and "))
}

/// The `<version>` of a `/source/<project>/<package>?view=info&parse=1`
/// answer, or `None` when it names none (a package with no parsable spec).
fn parse_sourceinfo_version(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<version>")? + "<version>".len()..];
    let version = rest[..rest.find("</version>")?].trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The version `pkg_name` has in the sources of `project`, read from the spec
/// OBS parsed, or `None` when it cannot be told. The version a linked project
/// ships is what a dependent's bound has to be met by, so this is how the
/// closure check learns it without checking the package out.
pub fn source_version(project: &str, pkg_name: &str) -> Option<String> {
    let xml = osc_api(&format!(
        "/source/{}/{}?view=info&parse=1",
        enc(project),
        enc(pkg_name)
    ))?;
    parse_sourceinfo_version(&xml)
}

#[cfg(test)]
mod sourceinfo_version_tests;

/// A warning for a package OBS reports `succeeded` whose result a dependent
/// cannot use, or `None` when nothing is wrong or it cannot be told. The test
/// is deterministic: the spec OBS holds is an unfinished skeleton (see
/// [`unfinished_spec_reason`]), and the RPMs it published are listed with
/// their sizes as evidence. A dependent installs such a package and still
/// finds its modules missing, which reads as "the dependency is missing".
pub fn empty_package_warning(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let spec_name = package_spec_name(project, pkg_name, Emit::quiet());
    let spec = osc_api(&format!(
        "/source/{}/{}/{}",
        enc(project),
        enc(pkg_name),
        enc(&spec_name)
    ))?;
    let reason = unfinished_spec_reason(&spec)?;
    let sizes = osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpm_sizes(&xml))
    .unwrap_or_default();
    Some(empty_package_message(pkg_name, project, &reason, &sizes))
}

fn empty_package_message(
    pkg: &str,
    project: &str,
    reason: &str,
    sizes: &[(String, u64)],
) -> String {
    let published = if sizes.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = sizes
            .iter()
            .map(|(f, b)| format!("{f}: {b} bytes"))
            .collect();
        format!(" (published: {})", list.join(", "))
    };
    format!(
        "{pkg} is built in {project}, but its spec there is an unfinished skeleton: {reason}, \
         so the RPMs it published are effectively empty{published}. A package that needs it \
         installs it and still cannot import it. Finish {pkg}'s spec, then rebuild it with \
         `auto-package build --project {project} --force <name>`"
    )
}

/// Package names to queue for building, from the RPM names a build or an
/// agent reports missing: see [`crate::depclass::triage_all`], here without
/// asking the distro (the names come from OBS saying the target has nothing
/// providing them). Callers that want to tell the user why a name was left out
/// use `triage_all` directly.
pub fn missing_package_names(rpm_names: &[String]) -> Vec<String> {
    crate::depclass::triage_all(rpm_names, &|_| false).0
}

/// Whether `name` is an RPM capability (a shared library such as
/// `libcuda.so.1()(64bit)`, a file path, an `rpmlib(...)` feature) rather than
/// a package name. OBS reports these as `nothing provides` too, but there is
/// no project to build for them: `libcuda.so.1` is the NVIDIA driver's own
/// library (convention 60), supplied at runtime, so queueing it as a package
/// can never succeed. The fix is on the package that requires it (an
/// `%global __requires_exclude`), which convention 60 tells the agent.
pub fn is_rpm_capability(name: &str) -> bool {
    let name = name.trim();
    if name.starts_with('/') || name.starts_with("rpmlib(") || name.contains("()") {
        return true;
    }
    // `lib<x>.so`, `.so.<n>`: not a name that merely contains ".so".
    name.match_indices(".so")
        .any(|(i, _)| matches!(name[i + 3..].chars().next(), None | Some('.') | Some('(')))
}

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
