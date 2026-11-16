//! The projects a repository builds against, transitively: its build paths,
//! the build paths of those in the same repository, and so on. OBS resolves
//! paths recursively, so a package a project two paths away ships is
//! installable by its builds, and counts as available there.
//!
//! Paths are followed per repository: a project's `openSUSE_Tumbleweed`
//! repository builds against Factory and its `16.0` one against Leap, so a
//! package only Leap ships is not available to the Tumbleweed build.

use super::*;

/// How many levels of build paths are followed from the root. Pairs reached at
/// the last level are listed; their own paths are not read.
pub const MAX_BUILD_PATH_DEPTH: usize = 8;

/// Bound on the `(project, repository)` pairs one walk visits, against a wide or
/// cyclic graph.
pub const MAX_BUILD_PATH_NODES: usize = 64;

/// A `(project, repository)` pair: a repository of a project, as a build path
/// names it.
pub type PathPair = (String, String);

/// Walk the `(project, repository)` pairs `start` leads to, depth first in the
/// order OBS expands paths: a path's own paths are visited before the next path
/// of its parent. So a first match in the result is the one OBS would pick, and
/// `start` is at depth 1.
///
/// The root project is never reached. Each pair is visited once (so a cycle or
/// a diamond ends), and at most `max_nodes` pairs are visited.
///
/// `reach(project, repository, depth, expand)` is called once per pair. `expand`
/// is false at the last level, where a pair's own paths are not wanted. It
/// answers the pair's item and the pairs its repository builds against, or
/// `None` when the pair is not there to be read: it is left out and its paths
/// are not followed. An `Err` stops the walk.
pub(crate) fn walk_path_pairs<T, E>(
    root: &str,
    start: Vec<PathPair>,
    max_depth: usize,
    max_nodes: usize,
    mut reach: impl FnMut(&str, &str, usize, bool) -> Result<Option<(T, Vec<PathPair>)>, E>,
) -> Result<Vec<(String, String, T)>, E> {
    let mut stack: Vec<(PathPair, usize)> = start.into_iter().rev().map(|pair| (pair, 1)).collect();
    let mut seen: std::collections::HashSet<PathPair> = std::collections::HashSet::new();
    let mut found = Vec::new();
    while let Some(((project, repository), depth)) = stack.pop() {
        if project == root || depth > max_depth {
            continue;
        }
        if seen.len() >= max_nodes {
            break;
        }
        if !seen.insert((project.clone(), repository.clone())) {
            continue;
        }
        let expand = depth < max_depth;
        let Some((item, paths)) = reach(&project, &repository, depth, expand)? else {
            continue;
        };
        found.push((project, repository, item));
        if expand {
            stack.extend(paths.into_iter().rev().map(|pair| (pair, depth + 1)));
        }
    }
    Ok(found)
}

/// The `(project, repository)` pairs `repository` of `project` builds against,
/// in the order OBS lists them.
pub fn build_path_pairs(
    project: &str,
    repository: &str,
    emit: Emit<'_>,
) -> Result<Vec<PathPair>, Error> {
    Ok(get_project_build_paths(project, emit)?
        .into_iter()
        .filter(|build_path| build_path.repository == repository)
        .map(|build_path| (build_path.project, build_path.path_repository))
        .collect())
}

/// Whether a failed read of the path project `project` at `depth` means that
/// project is not there: a 404, or a 403 (a project we only read from may refuse
/// us). Only the paths below the root's own direct paths count: a direct path
/// that cannot be read fails the walk, and so does a transient failure.
pub(crate) fn absent_path_project(reason: &str, transient: bool, depth: usize) -> bool {
    depth > 1 && !transient && lookup_says_absent(reason, true)
}

/// Settles one failed read of a path project. An absent project (see
/// [`absent_path_project`]) is warned about once per process and left out:
/// `Ok(None)`. Anything else is the error.
fn skip_absent<T>(
    result: Result<T, Error>,
    project: &str,
    depth: usize,
    emit: Emit<'_>,
) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if absent_path_project(&e.to_string(), e.is_transient(), depth) => {
            static WARNED: std::sync::OnceLock<
                std::sync::Mutex<std::collections::HashSet<String>>,
            > = std::sync::OnceLock::new();
            let fresh = WARNED
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(project.to_string());
            if fresh {
                crate::proc::report_failure_warning(
                    emit.reporter,
                    &Event::Warning {
                        step: emit.step,
                        message: format!("{e}; treating {project} as not carrying the packages"),
                    },
                );
            }
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The `(project, repository, item)` of every pair `project`'s `repository`
/// reaches, in OBS order (see [`walk_path_pairs`]). `paths_of` reads a
/// repository's build paths and `item_of` what a path project carries at a
/// depth. The two seams let the policy run against fakes.
pub(crate) fn linked_walk<T>(
    project: &str,
    repository: &str,
    emit: Emit<'_>,
    mut paths_of: impl FnMut(&str, &str) -> Result<Vec<PathPair>, Error>,
    mut item_of: impl FnMut(&str, usize) -> Result<T, Error>,
) -> Result<Vec<(String, String, T)>, Error> {
    let start = paths_of(project, repository)?;
    let mut absent: std::collections::HashSet<String> = std::collections::HashSet::new();
    walk_path_pairs(
        project,
        start,
        MAX_BUILD_PATH_DEPTH,
        MAX_BUILD_PATH_NODES,
        |p, r, depth, expand| {
            if absent.contains(p) {
                return Ok(None);
            }
            let Some(item) = skip_absent(item_of(p, depth), p, depth, emit)? else {
                absent.insert(p.to_string());
                return Ok(None);
            };
            let paths = if expand {
                match skip_absent(paths_of(p, r), p, depth, emit)? {
                    Some(paths) => paths,
                    None => {
                        absent.insert(p.to_string());
                        return Ok(None);
                    }
                }
            } else {
                Vec::new()
            };
            Ok(Some((item, paths)))
        },
    )
}

/// The first pair of each project in `found`, with that pair's item.
fn first_of_each_project<T>(found: Vec<(String, String, T)>) -> Vec<(String, T)> {
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter_map(|(project, _, item)| seen.insert(project.clone()).then_some((project, item)))
        .collect()
}

/// The listings of every project `repository` of `project` builds against,
/// transitively, nearest first. Each listing comes from
/// [`list_project_packages`], so a project listed earlier in the run is not
/// fetched again. An absent path project is left out (see
/// [`absent_path_project`]). Publication and enablement of a package are not
/// checked: a package a path project lists counts as there.
pub fn linked_project_listings(
    project: &str,
    repository: &str,
    emit: Emit<'_>,
) -> Result<Vec<(String, Arc<std::collections::HashSet<String>>)>, Error> {
    let found = linked_walk(
        project,
        repository,
        emit,
        |p, r| build_path_pairs(p, r, emit),
        |p, _| list_project_packages(p, emit),
    )?;
    Ok(first_of_each_project(found))
}

/// The names of every project `repository` of `project` builds against,
/// transitively, nearest first, without listing their packages.
pub fn linked_project_names(
    project: &str,
    repository: &str,
    emit: Emit<'_>,
) -> Result<Vec<String>, Error> {
    let found = linked_walk(
        project,
        repository,
        emit,
        |p, r| build_path_pairs(p, r, emit),
        |_, _| Ok(()),
    )?;
    Ok(first_of_each_project(found)
        .into_iter()
        .map(|(project, ())| project)
        .collect())
}
