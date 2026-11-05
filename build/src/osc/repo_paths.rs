//! The repository paths of a project, cached, and whether a project has
//! repositories.

use super::*;
use crate::osc::poll::*;
use crate::osc::query::*;

/// One `<path>` of a project's `<repository>`: the repository `repository` of
/// the project builds against `path_repository` of `project`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPath {
    pub repository: String,
    pub project: String,
    pub path_repository: String,
}

/// `get_project_repo_paths` answers by project, shared for a few minutes.
static REPO_PATHS_CACHE: std::sync::Mutex<Option<HashMap<String, (Instant, Vec<BuildPath>)>>> =
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

/// Return the build paths of each repository of the OBS project `project`.
///
/// Parses `osc meta prj <project>` XML and extracts every
/// `<path project="..." repository="..."/>` element, keeping the repository it
/// belongs to: a project has several repositories (`openSUSE_Tumbleweed`,
/// `16.0`, ...), each building against its own paths. Callers follow the
/// paths of the repository they build for only, see
/// [`super::build_paths::build_path_pairs`]. A package whose build in the
/// linked repository is disabled or broken there is still treated as
/// available: no per-repo build-status query is made for it.
///
/// A failed query is an `Err`, never an empty list: empty reads as "no build
/// paths", which widens the set of packages reported missing.
pub fn get_project_build_paths(project: &str, emit: Emit<'_>) -> Result<Vec<BuildPath>, Error> {
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
        match capture_query(
            &["meta", "prj", project],
            &anywhere(),
            emit,
            OSC_QUERY_TIMEOUT,
        )
        .map_err(during(cmd.clone()))
        {
            Ok(out) if out.success() => {
                let paths = parse_build_paths(&out.stdout);
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

/// The `(project, repository)` pairs of every build path of `project`, whatever
/// repository it belongs to, each pair once. A caller that follows the paths of
/// one repository uses [`get_project_build_paths`] instead.
pub fn get_project_repo_paths(
    project: &str,
    emit: Emit<'_>,
) -> Result<Vec<(String, String)>, Error> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for build_path in get_project_build_paths(project, emit)? {
        let pair = (build_path.project, build_path.path_repository);
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    Ok(pairs)
}

/// Parsing half of [`get_project_build_paths`], split out so it is testable
/// without shelling out to `osc`.
pub(super) fn parse_build_paths(xml: &str) -> Vec<BuildPath> {
    crate::osc::branch::parse_project_info(xml)
        .repositories
        .into_iter()
        .flat_map(|repo| {
            repo.paths
                .into_iter()
                .map(move |(project, path_repository)| BuildPath {
                    repository: repo.name.clone(),
                    project,
                    path_repository,
                })
        })
        .collect()
}

/// The `(project, repository)` pairs of every `<path>` in `xml`, whatever
/// repository it belongs to, each pair once.
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
    let out = capture_query(
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
