//! Build status of packages: the status of one package and of a project, and
//! the parse of the result XML.

use super::*;
use crate::osc::poll::*;
use crate::osc::query::*;
use crate::osc::results::*;

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

/// [`get_build_status`] for a checkout that is about to be judged against the
/// spec on disk: a built verdict counts only when the job history shows it was
/// built from the revision now on the server, which is the revision a checkout
/// holds. A verdict of an earlier revision reads as `Unknown`, as it does right
/// after a commit.
pub fn get_build_status_confirmed(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Result<BuildStatus, Error> {
    match get_build_status_xml(project, pkg_name, repo, arch, true, None)? {
        Some(status) => Ok(status),
        None => Ok(no_result_status()),
    }
}

/// [`get_build_status`] for a package whose sources were just committed: a
/// built verdict only counts once the job history shows it came from the
/// revision now on the server (see [`obs_status::Evidence::confirm`]), so the
/// previous revision's `failed`/`succeeded` is never mistaken for the result.
/// Without the XML answer there is no job history to check, so the status is
/// `Unknown`. `committed` is when the commit was done: the `_result` document
/// is never older than that (see [`ResultsCache::get`]).
pub(super) fn get_build_status_after_commit(
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
    get_overall_build_status_within(project, pkg_name, repo, arch, OVERALL_MAX_WAIT)
}

/// How long [`get_overall_build_status`] waits for the other architectures.
pub const OVERALL_MAX_WAIT: Duration = Duration::from_secs(1800);

/// [`get_overall_build_status`] with the wait bounded by `max_wait`: zero reads
/// the results once, for a caller that has already waited for them.
pub fn get_overall_build_status_within(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    max_wait: Duration,
) -> Result<(BuildStatus, Option<String>), Error> {
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
        if !active || start.elapsed() >= max_wait || crate::proc::shutting_down() {
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
pub(super) fn overall_status(
    entries: &[obs_status::ResultEntry],
    pkg_name: &str,
) -> (BuildStatus, Option<String>) {
    let flavor_prefix = format!("{pkg_name}:");
    let statuses: Vec<(BuildStatus, String)> = entries
        .iter()
        .filter(|e| e.pkg == pkg_name || e.pkg.starts_with(&flavor_prefix))
        // A port is judged by the arch check, not by the package's own status.
        .filter(|e| !crate::arch_check::is_port_repo(&e.repo))
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
pub(super) fn get_build_status_from_results_table(
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
pub(super) fn parse_single_build_status(
    stdout: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> BuildStatus {
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
pub(super) fn strip_result_suffixes(head: &str) -> &str {
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
pub(super) fn status_from_osc_text(text: &str) -> BuildStatus {
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
pub(super) fn fold_flavor_statuses(
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
pub(super) fn project_statuses_from_xml(
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

/// Every result of `pkg_name` in `project` (all repositories, architectures
/// and multibuild flavors), as the `_result` document gives them. Used by the
/// other-architecture check, which reads the branch and the origin the same way.
pub fn get_package_results(
    project: &str,
    pkg_name: &str,
) -> Result<Vec<obs_status::ResultEntry>, String> {
    get_package_results_if_present(project, pkg_name)?.ok_or_else(|| {
        format!(
            "/build/{}/_result?package={}: HTTP 404: no such project or package",
            enc(project),
            enc(pkg_name)
        )
    })
}

/// [`get_package_results`], but `None` when OBS answers 404: the project or the
/// package is not there, which a baseline may well be.
pub fn get_package_results_if_present(
    project: &str,
    pkg_name: &str,
) -> Result<Option<Vec<obs_status::ResultEntry>>, String> {
    check_obs_name("project", project).map_err(|e| e.to_string())?;
    check_obs_name("pkg_name", pkg_name).map_err(|e| e.to_string())?;
    let path = format!(
        "/build/{}/_result?package={}&multibuild=1",
        enc(project),
        enc(pkg_name)
    );
    let xml = match osc_api_checked(&path) {
        Ok(xml) => xml,
        Err(ApiFailure::Http(404, _)) => return Ok(None),
        Err(ApiFailure::Http(code, msg)) => return Err(format!("{path}: HTTP {code}: {msg}")),
        Err(ApiFailure::Other(msg)) => return Err(msg),
    };
    obs_status::try_parse_results(&xml)
        .map(Some)
        .map_err(|e| e.to_string())
}
