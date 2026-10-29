//! The package listing of a project: the cached list, its generation, and the
//! fetch and parse of the listing.

use super::*;
use crate::osc::results::*;

pub(super) static PROJECT_PACKAGES_CACHE: OnceLock<
    Mutex<HashMap<String, Arc<std::collections::HashSet<String>>>>,
> = OnceLock::new();

/// Per-project count of invalidations. A fetch that started before an
/// invalidation holds a listing that may predate the package just committed;
/// the reader compares the generation it saw before fetching and drops the
/// result if it moved, instead of caching the stale set for the whole run.
static PROJECT_PACKAGES_GENERATION: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

pub(super) fn project_generation(project: &str) -> u64 {
    PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project)
        .copied()
        .unwrap_or(0)
}

/// Insert `pkgs` for `project` unless it was invalidated since `generation`
/// was read. The generation is checked under the cache lock, and
/// invalidation bumps it before taking that lock, so no interleaving lets a
/// stale set land after the removal.
pub(super) fn insert_if_current(
    project: &str,
    generation: u64,
    pkgs: &Arc<std::collections::HashSet<String>>,
) {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if project_generation(project) == generation {
        guard.insert(project.to_owned(), Arc::clone(pkgs));
    }
}

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip — callers look names up once per dependency,
/// so without caching a single batch can mean dozens of `osc ls` round-trips.
/// Callers that successfully commit a *new* package into `project` MUST call
/// [`invalidate_project_packages_cache`] afterward: a long `auto-package
/// build` run adds packages to the same project continuously, and this
/// cache has no TTL and no other invalidation path. Without that call, the
/// very first lookup permanently freezes the answer for the rest of the
/// process — every package built after it would look like it still doesn't
/// exist to every dependent package's availability check, for the entire
/// remainder of the run, no matter how many more succeed.
///
/// Errors when the listing cannot be fetched, so a caller cannot mistake
/// "OBS did not answer" for "the package is absent".
pub fn try_list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, String> {
    list_project_packages(project, emit)
        .map_err(|e| format!("could not list packages of {project}: {e}"))
}

/// [`try_list_project_packages`] keeping the failure as an [`Error`], so that
/// its transience (a timeout, an OBS 5xx) reaches the caller.
pub fn list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, Error> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pkgs) = guard.get(project) {
            return Ok(Arc::clone(pkgs));
        }
    }

    let generation = project_generation(project);
    // A failed listing must not be cached: an empty set would make every
    // package look absent from OBS for the rest of the run. Try up to three
    // times with a short backoff, then report the error so the next call
    // tries again.
    let mut listing = fetch_project_packages(project, emit);
    for backoff in [1, 2] {
        if listing.is_ok() || crate::proc::shutting_down() {
            break;
        }
        std::thread::sleep(Duration::from_secs(backoff));
        listing = fetch_project_packages(project, Emit::quiet());
    }
    let pkgs = listing.map(Arc::new)?;
    insert_if_current(project, generation, &pkgs);
    Ok(pkgs)
}

/// Drop `project`'s cached package listing so the next [`try_list_project_packages`]
/// call re-fetches it. Call this right after successfully building/committing
/// a package into `project` — see [`try_list_project_packages`]'s doc comment for
/// why skipping this silently breaks every later availability check for the
/// rest of the run, not just the one for the package just built.
pub fn invalidate_project_packages_cache(project: &str) {
    // The sources changed, so the shared build results of the project may too.
    RESULTS_CACHE.invalidate(project);
    forget_published_binaries();
    // Bump first: a fetch in flight must see the change even if it reaches
    // `insert_if_current` before the removal below.
    *PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project.to_owned())
        .or_insert(0) += 1;
    if let Some(cache) = PROJECT_PACKAGES_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(project);
    }
}

pub(super) fn fetch_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<std::collections::HashSet<String>, Error> {
    if !is_valid_obs_name(project) {
        return Err(Error::NotActionable(format!(
            "refusing to use project {project:?}: not a valid OBS name"
        )));
    }
    let out = proc::capture_with_timeout(
        "osc",
        &["ls", project],
        &anywhere(),
        emit,
        // A whole distro project (openSUSE:Factory) is a long listing.
        Duration::from_secs(180),
    )
    .map_err(during(format!("ls {project}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("ls {project}"),
            code: out.code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(parse_package_listing(&out.stdout))
}

/// Package names from `osc ls <project>`, in the spelling OBS stores them
/// (OBS names are case-sensitive: `python-APScheduler`). A multibuild package
/// is listed as `<source>:<flavor>` (`apache-arrow:python-pyarrow`) and its
/// flavor is itself a package that gets built and published (python313-pyarrow),
/// so the flavor name counts as available too — otherwise a dependency Factory
/// already ships looks absent and gets queued to be built again.
pub(super) fn parse_package_listing(stdout: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for name in stdout.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((_, flavor)) = name.split_once(':')
            && flavor.starts_with("python")
        {
            out.insert(flavor.to_string());
        }
        out.insert(name.to_string());
    }
    out
}
