//! The repository paths of a project, cached, and whether a project has
//! repositories.

use super::*;
use crate::osc::poll::*;
use crate::osc::query::*;

type RepoPaths = Vec<(String, String)>;

/// `get_project_repo_paths` answers by project, shared for a few minutes.
static REPO_PATHS_CACHE: std::sync::Mutex<Option<HashMap<String, (Instant, RepoPaths)>>> =
    std::sync::Mutex::new(None);

/// Forget a project's cached build paths: its meta was just written.
pub(crate) fn invalidate_repo_paths(project: &str) {
    if let Some(c) = REPO_PATHS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        c.remove(project);
    }
}

/// Return the `(project, repository)` pairs the OBS project's repositories
/// build against.
///
/// Parses `osc meta prj <project>` XML and extracts every
/// `<path project="..." repository="..."/>` element. These are the upstream
/// projects and repositories (e.g. `openSUSE:Factory`, repository
/// `snapshot`) whose packages are considered "already available" and do not
/// need to be built.
///
/// The repository half of each pair is deliberately not used by either
/// caller to filter further ([`select_target`]'s resolvability check and
/// the CLI's queue filter both just list every package name in
/// `path_project`, regardless of that package's build status *in* the named
/// repository) — a package whose build in the linked repo is disabled or
/// broken there is currently still treated as available here. Narrowing that
/// would need a per-repo build-status query in addition to this one; nothing
/// does that today.
///
/// A failed query is an `Err`, never an empty list: empty reads as "no build
/// paths", which widens the set of packages reported missing.
pub fn get_project_repo_paths(
    project: &str,
    emit: Emit<'_>,
) -> Result<Vec<(String, String)>, Error> {
    check_obs_name("project", project)?;
    // The build paths of a project change when someone edits its meta, not during a
    // run: one answer is shared for a few minutes. A timed-out query is tried
    // again: a single `osc meta prj` killed after 60 s (two parallel runs, a loaded
    // machine) ended the root build of open-webui with 'could not read build paths'.
    const TTL: Duration = Duration::from_secs(180);
    if let Some((at, paths)) = REPO_PATHS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(project))
        && at.elapsed() < TTL
    {
        return Ok(paths.clone());
    }
    let cmd = format!("meta prj {project}");
    let mut last: Option<Error> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep_interruptible(Duration::from_secs(5 * attempt));
        }
        match proc::capture_with_timeout(
            "osc",
            &["meta", "prj", project],
            &anywhere(),
            emit,
            OSC_QUERY_TIMEOUT,
        )
        .map_err(during(cmd.clone()))
        {
            Ok(out) if out.success() => {
                let paths = parse_project_repo_paths(&out.stdout);
                REPO_PATHS_CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert_with(Default::default)
                    .insert(project.to_string(), (Instant::now(), paths.clone()));
                return Ok(paths);
            }
            // A server answer that is not success (unknown project, no permission)
            // does not change by asking again.
            Ok(out) => {
                return Err(Error::OscFailed {
                    cmd,
                    code: out.code,
                    stderr: out.stderr,
                });
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("three attempts were made"))
}

/// Parsing half of [`get_project_repo_paths`], split out so it is testable
/// without shelling out to `osc`.
pub(super) fn parse_project_repo_paths(xml: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for chunk in xml.split("<path ").skip(1) {
        let tag = &chunk[..chunk.find("/>").unwrap_or(chunk.len())];
        let project_name = crate::obs_xml::attr(tag, "project");
        let repo_name = crate::obs_xml::attr(tag, "repository");
        if let (Some(p), Some(r)) = (project_name, repo_name) {
            let pair = (p, r);
            if !paths.contains(&pair) {
                paths.push(pair);
            }
        }
    }
    paths
}

/// Does the OBS project meta list at least one `<repository>` element?
///
/// A project with no repositories cannot build anything — every `osc build`
/// against it fails immediately with `no repositories defined` (see
/// [`FailureKind::NoRepositories`]). Catching that at the top of a run gives
/// the user a clear, actionable error up front instead of a per-package
/// `NotActionable` after each queued package has already been checked out.
///
/// Returns `Ok(true)` when at least one `<repository name="...">` is present,
/// `Ok(false)` when the project meta loaded but has none, and `Err` when the
/// `osc meta prj` call itself failed (project doesn't exist, auth broken, ...).
pub fn project_has_repositories(project: &str, emit: Emit<'_>) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("meta prj {project}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta prj {project}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(parse_has_repository(&out.stdout))
}

/// Parsing half of [`project_has_repositories`], split out so it is testable
/// without shelling out to `osc`.
pub(super) fn parse_has_repository(xml: &str) -> bool {
    xml.contains("<repository ") || xml.contains("<repository\n") || xml.contains("<repository\t")
}
