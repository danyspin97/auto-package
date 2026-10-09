//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

/// Where `osc` runs when the command is not scoped to a checkout.
fn anywhere() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Fired at most once per process the first time `rpmspec --parse` is
/// invoked, surfacing the known host-vs-target-repo macro-mismatch gap the
/// 2026-10-06 external review flagged. Behavior is unchanged — this just
/// tells the user to treat a passing `rpmspec` as a cheap syntax check, not
/// as a reproduction of what the OBS worker will do.
fn emit_host_macro_warning(emit: Emit<'_>) {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: "`rpmspec --parse` validates specs using host RPM macros, not \
                 the target OBS build repo's macros — a clean parse here does \
                 not guarantee the OBS worker will see the same expansions \
                 (e.g. `%python3_sitelib`, `%python_module`, `%suse_version`). \
                 Treat it as a syntax check, not a reproduction."
                .to_string(),
        });
    });
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

    /// One or more RPM dependencies could not be resolved. `names` holds the
    /// raw RPM package names (e.g. `python314-puccinialin`) the caller
    /// should queue for building and retry this one against; `detail` holds
    /// the verbatim `nothing provides ...`/`(got version ...)` lines behind
    /// them, when OBS gave any — the only way to tell a genuinely missing
    /// package apart from a version constraint this spec itself wrote that
    /// the repo's one available version can never satisfy (e.g.
    /// `setuptools < 70.0.0` when only 80.9.0 is shipped).
    #[error("unresolvable RPM dependencies: {}", .names.join(", "))]
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },

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

    /// Decode one of `osc results <project>`'s single-letter per-package
    /// status codes (see its own "Legend:" footer).
    fn from_code(c: char) -> Self {
        match c {
            '.' => BuildStatus::Succeeded,
            ' ' => BuildStatus::Disabled,
            'U' => BuildStatus::Unresolvable,
            'F' => BuildStatus::Failed,
            'B' => BuildStatus::Broken,
            'b' => BuildStatus::Blocked,
            '%' | 'd' | 'S' => BuildStatus::Building,
            's' => BuildStatus::Scheduled,
            'x' => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
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
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },
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
    let names = extract_unresolvable(log);
    if !names.is_empty() {
        let detail = extract_unresolvable_detail(log);
        return FailureKind::UnresolvableDeps { names, detail };
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
        let pkg = pkg_str.split_whitespace().next().unwrap_or(pkg_str);
        let pkg = clean_unresolvable_name(pkg);
        if !pkg.is_empty() && seen.insert(pkg.to_string()) {
            result.push(pkg.to_string());
        }
    }
    result
}

/// Strip decoration OBS leaves on a "nothing provides <name>" token so the
/// result is an actual RPM name: a `(`/`)` from a rich `(A or B)` requires
/// group (e.g. `python313-duckdb)`), and a trailing `[flavor]` multibuild
/// annotation (e.g. `python314-pybind11[global]`). Left in place, these
/// aren't valid RPM or PyPI names, so `strip_rpm_prefix` and PyPI lookup
/// both miss and the dependency silently never gets queued to build.
fn clean_unresolvable_name(pkg: &str) -> &str {
    let pkg = pkg.trim_start_matches('(').trim_end_matches(')');
    match pkg.rfind('[') {
        Some(idx) if pkg.ends_with(']') => &pkg[..idx],
        _ => pkg,
    }
}

/// Extract the verbatim `nothing provides ...` lines from a build log or
/// `osc results --verbose` output, together with any `(got version ...)`
/// continuation line immediately following one — unlike [`extract_unresolvable`],
/// which throws away everything but the bare package name, this keeps the
/// version constraint and the version OBS actually has, e.g.:
///   `nothing provides python314-setuptools < 70.0.0`
///   `(got version 80.9.0-3.3)`
/// That pairing is the only signal that distinguishes a genuinely missing
/// package from a version constraint the spec itself wrote that the repo's
/// one available version can never satisfy — `extract_unresolvable`'s bare
/// name alone looks identical in both cases.
fn extract_unresolvable_detail(log: &str) -> Vec<String> {
    let mut result = Vec::new();
    let lines: Vec<&str> = log.lines().collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        if !line.starts_with("nothing provides")
            && !line.starts_with("unresolvable: nothing provides")
        {
            continue;
        }
        result.push(line.to_string());
        if let Some(next) = lines.get(i + 1) {
            let next = next.trim();
            if next.starts_with('(') {
                result.push(next.to_string());
            }
        }
    }
    result
}

#[cfg(test)]
mod extract_unresolvable_tests {
    use super::*;

    /// Regression test: a rich `(A or B)` requires group leaves a stray `)`
    /// on the last alternative's "nothing provides" line, and a multibuild
    /// flavor annotation leaves a trailing `[flavor]`. Either one turns the
    /// extracted name into something neither `strip_rpm_prefix` nor a PyPI
    /// lookup recognizes, so the dependency silently drops out of the queue
    /// instead of being built.
    #[test]
    fn strips_boolean_group_parens_and_flavor_brackets() {
        let log = "\
unresolvable: nothing provides python314-pytest-rerunfailures
unresolvable: nothing provides python313-duckdb)
nothing provides python314-pybind11[global] needed by python314-fastar
nothing provides (python313-foo
";
        let names = extract_unresolvable(log);
        assert_eq!(
            names,
            vec![
                "python314-pytest-rerunfailures",
                "python313-duckdb",
                "python314-pybind11",
                "python313-foo",
            ]
        );
    }

    #[test]
    fn leaves_ordinary_names_untouched() {
        assert_eq!(
            clean_unresolvable_name("python314-duckdb"),
            "python314-duckdb"
        );
    }
}

/// RPM package name for a Python package: `python-<name>` (lowercased).
///
/// This is only a guess at the real name for a package that does not exist
/// yet — openSUSE's actual Python packaging has no single deterministic rule
/// for how a PyPI name's `-`/`_`/`.` characters map to the RPM name: some
/// real packages convert underscores to hyphens (PyPI's `pydantic_core` is
/// `python-pydantic-core`, `mistral_common` is `python-mistral-common`),
/// while just as many keep the underscore as-is (`typing_extensions`,
/// `setuptools_scm`, `mypy_extensions`, `prompt_toolkit`, `async_timeout` are
/// all packaged with the literal underscore), and dotted names always keep
/// the dot (`ruamel.yaml`, `jaraco.classes`, `backports.zstd`) — confirmed by
/// checking real openSUSE package listings, not derivable from the PyPI name
/// alone. Never compare this against a real package list with `==`/
/// `.contains()` for an existence check — use [`fuzzy_key`] on both sides, and
/// when it matches, use the real listing's spelling, not this guess.
pub fn rpm_name(pypi_name: &str) -> String {
    format!("python-{}", pypi_name.to_lowercase())
}

/// A comparison-only key for matching two RPM/PyPI package names that might
/// spell the same project with a different `-`/`_`/`.` choice (see
/// [`rpm_name`] for why no single guess is reliable): lowercase with every
/// `-`, `_`, and `.` removed entirely, so `pydantic_core`, `pydantic-core`,
/// and `pydantic.core` all collapse to the same key. Two distinct real PyPI
/// projects differing only in separator choice are not a realistic
/// collision to worry about here.
///
/// Use this to test "does a package matching this name already exist",
/// never to construct a name to actually build or commit under — when a
/// match is found, use the real listing's own spelling instead.
pub fn fuzzy_key(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod fuzzy_key_tests {
    use super::*;

    #[test]
    fn treats_hyphen_underscore_and_dot_as_equivalent() {
        assert_eq!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic_core")
        );
        assert_eq!(
            fuzzy_key("python-ruamel.yaml"),
            fuzzy_key("python-ruamel-yaml")
        );
        assert_eq!(
            fuzzy_key("Python-Backports.Zstd"),
            fuzzy_key("python-backports_zstd")
        );
    }

    #[test]
    fn distinct_names_still_produce_distinct_keys() {
        assert_ne!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic")
        );
    }
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
/// [`get_available_packages`] both just list every package name in
/// `path_project`, regardless of that package's build status *in* the named
/// repository) — a package whose build in the linked repo is disabled or
/// broken there is currently still treated as available here. Narrowing that
/// would need a per-repo build-status query in addition to this one; nothing
/// does that today.
pub fn get_project_repo_paths(project: &str, emit: Emit<'_>) -> Vec<(String, String)> {
    let Ok(out) = proc::capture("osc", &["meta", "prj", project], &anywhere(), emit) else {
        return vec![];
    };
    if !out.success() {
        return vec![];
    }

    parse_project_repo_paths(&out.stdout)
}

/// Parsing half of [`get_project_repo_paths`], split out so it is testable
/// without shelling out to `osc`.
fn parse_project_repo_paths(xml: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for chunk in xml.split("<path ").skip(1) {
        let tag = &chunk[..chunk.find("/>").unwrap_or(chunk.len())];
        let project_name = extract_xml_attr(tag, "project");
        let repo_name = extract_xml_attr(tag, "repository");
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
    let out = proc::capture("osc", &["meta", "prj", project], &anywhere(), emit)?;
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
fn parse_has_repository(xml: &str) -> bool {
    xml.contains("<repository ") || xml.contains("<repository\n") || xml.contains("<repository\t")
}

/// Extract `attr="value"` from a fragment of XML attribute text.
fn extract_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Return the union of source packages in `project` and all its configured
/// build/path projects (from `osc meta prj`).
///
/// This is the correct set to check for resolvability: a package is available
/// for building if it exists in the user's own project OR in any project that
/// the user's project is configured to build against.
pub fn get_available_packages(project: &str, emit: Emit<'_>) -> std::collections::HashSet<String> {
    let mut all = list_project_packages(project, emit);
    for (path_project, _repo) in get_project_repo_paths(project, emit) {
        all.extend(list_project_packages(&path_project, emit));
    }
    all
}

static PROJECT_PACKAGES_CACHE: OnceLock<Mutex<HashMap<String, std::collections::HashSet<String>>>> =
    OnceLock::new();

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip — this is called once per dependency in the
/// CLI's `pending_deps` availability filter, so without caching a single
/// batch can mean dozens of `osc ls` round-trips. Callers that successfully
/// commit a *new* package into `project` MUST call
/// [`invalidate_project_packages_cache`] afterward: a long `auto-package
/// build` run adds packages to the same project continuously, and this
/// cache has no TTL and no other invalidation path. Without that call, the
/// very first lookup permanently freezes the answer for the rest of the
/// process — every package built after it would look like it still doesn't
/// exist to every dependent package's availability check, for the entire
/// remainder of the run, no matter how many more succeed.
pub fn list_project_packages(project: &str, emit: Emit<'_>) -> std::collections::HashSet<String> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap();
        if let Some(pkgs) = guard.get(project) {
            return pkgs.clone();
        }
    }

    let pkgs = fetch_project_packages(project, emit);
    cache
        .lock()
        .unwrap()
        .insert(project.to_owned(), pkgs.clone());
    pkgs
}

/// Drop `project`'s cached package listing so the next [`list_project_packages`]
/// call re-fetches it. Call this right after successfully building/committing
/// a package into `project` — see [`list_project_packages`]'s doc comment for
/// why skipping this silently breaks every later availability check for the
/// rest of the run, not just the one for the package just built.
pub fn invalidate_project_packages_cache(project: &str) {
    if let Some(cache) = PROJECT_PACKAGES_CACHE.get() {
        cache.lock().unwrap().remove(project);
    }
}

#[cfg(test)]
mod project_packages_cache_tests {
    use super::*;

    /// Regression test for the bug where a long run permanently froze every
    /// package's availability at whatever the cache first saw: builds z3
    /// -solver, pydantic-core, etc. later in the same run kept looking
    /// missing to every dependent package no matter how many succeeded,
    /// because nothing ever dropped the stale entry.
    #[test]
    fn invalidate_drops_the_cached_entry() {
        let project = "ap-test-cache-invalidation-project";
        let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        cache
            .lock()
            .unwrap()
            .insert(project.to_string(), std::collections::HashSet::new());
        assert!(cache.lock().unwrap().contains_key(project));

        invalidate_project_packages_cache(project);

        assert!(!cache.lock().unwrap().contains_key(project));
    }

    /// A project this call never touched must not be affected — invalidation
    /// is per-project, not a blanket cache flush of every project this
    /// process has ever looked at.
    #[test]
    fn invalidate_does_not_touch_the_cache_before_anything_is_in_it() {
        // Just needs to not panic when the cache is empty or lacks the key.
        invalidate_project_packages_cache("ap-test-project-never-cached");
    }
}

/// Enumerate the files committed to an OBS package.
///
/// Returns `None` when `osc ls` fails (project/package doesn't exist, auth
/// broken) rather than panicking, same as [`cat_file`]: the update sweep
/// uses that `None` to skip the package and record a SpecFetchFailed instead
/// of killing the whole scan.
///
/// Used so the update sweep can discover the real `.spec` filename rather
/// than guess `<pkg>.spec` — AppCo's versioned-library convention gives one
/// OBS package per upstream version (`pytorch+2.4`, `pytorch+2.5`), each
/// holding a plain `<base-name>.spec` (`pytorch.spec`) that has no relation
/// to the OBS package name. The guess silently missed every one of these
/// before (Oct 6 review finding #3).
pub fn list_package_files(project: &str, pkg: &str, emit: Emit<'_>) -> Option<Vec<String>> {
    let out = proc::capture("osc", &["ls", project, pkg], &anywhere(), emit).ok()?;
    if !out.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Fetch a single file's contents from an OBS package without checking out.
///
/// Returns `None` when the package doesn't exist, the file isn't in it, or
/// `osc cat` otherwise fails — the update sweep uses that `None` to skip the
/// package instead of blowing up the whole run on a stray missing spec.
pub fn cat_file(project: &str, pkg: &str, filename: &str, emit: Emit<'_>) -> Option<String> {
    let out = proc::capture("osc", &["cat", project, pkg, filename], &anywhere(), emit).ok()?;
    if !out.success() {
        return None;
    }
    Some(out.stdout)
}

fn fetch_project_packages(project: &str, emit: Emit<'_>) -> std::collections::HashSet<String> {
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
    let out = proc::capture(
        "osc",
        &["vc", "-m", "Initial packaging"],
        checkout_dir,
        emit,
    )?;
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
/// Creates it by pushing package meta directly (`osc meta pkg ... -F -`,
/// which the server creates implicitly if it doesn't exist yet) rather than
/// `osc mkpac`. `mkpac` infers "already registered" from local working-copy
/// state — a stray directory left over from a previous, incomplete attempt,
/// or a project checkout that still remembers the package from one — and
/// happily reports success without the package ever having been created on
/// the server. That false success surfaces much later as a 404 on `osc ci`.
/// Pushing meta directly has no such local state to get confused by: the
/// server is the only thing that decides whether the package exists.
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

    let meta = format!(
        "<package name=\"{pkg_name}\" project=\"{project}\">\n  \
         <title/>\n  <description/>\n</package>\n"
    );
    let meta_file = work_dir.join(format!(".{pkg_name}.meta.xml"));
    fs::write(&meta_file, &meta).map_err(|e| Error::OscFailed {
        cmd: format!("meta pkg {project} {pkg_name} (writing temp meta file)"),
        code: -1,
        stderr: e.to_string(),
    })?;

    let out = proc::capture(
        "osc",
        &[
            "meta",
            "pkg",
            project,
            pkg_name,
            "-F",
            &meta_file.to_string_lossy(),
        ],
        work_dir,
        emit,
    );
    let _ = fs::remove_file(&meta_file);
    let out = out?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta pkg {project} {pkg_name} -F -"),
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
        let out = osc_up_recovering(&checkout_dir, emit)?;

        if !out.success() {
            // 404 means `ensure_package`'s meta push hasn't propagated to
            // this checkout's view of the server yet — use the directory
            // as-is rather than failing on a lag that resolves itself.
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

    // Every `osc co` into the same directory rewrites the shared
    // `<project>/.osc/_packages` file (write `.new`, rename). With several
    // packages checking out in parallel (`--jobs`, or roots added from the
    // TUI) the renames race and one fails with FileNotFoundError, so
    // serialise checkouts process-wide.
    static CHECKOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CHECKOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
    let out = osc_up_recovering(checkout_dir, emit)?;
    if !out.success() {
        // 404 means `ensure_package`'s meta push hasn't propagated to this
        // checkout's view of the server yet — the directory is valid,
        // nothing to pull.
        let benign = [
            "nothing to update",
            "At revision",
            "Skipping",
            "404",
            "Not Found",
        ];
        if !benign
            .iter()
            .any(|b| out.stdout.contains(b) || out.stderr.contains(b))
        {
            return Err(Error::OscFailed {
                cmd: "up".to_string(),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Run `osc up` in `checkout_dir`, recovering in-place from the two known
/// self-inflicted working-copy failure modes before handing the (possibly
/// still-failing) result back to the caller: a prior run that died
/// mid-`up`/`ci` leaving `.osc/` inconsistent ([`is_inconsistent_wc_error`],
/// fixed by [`repair_wc`]), and an untracked local file colliding with a
/// same-named file the server gained in the meantime
/// ([`changes_conflict_file`], fixed by moving the local one aside).
///
/// Shared by [`checkout`]'s reuse path and [`pull`] so both get the same
/// recovery — `checkout`'s "osc up" on an existing directory is exactly the
/// same command `pull` runs, and the same interrupted-run history that leaves
/// `.osc/` inconsistent or a stray untracked `.changes` file behind can be hit
/// from either entry point, not just the one a fix happened to land in first.
/// Returns the final `osc up`'s output either way — success or not — so each
/// caller can keep interpreting failure (which stderr patterns are benign,
/// whether 404 is a non-error) in its own, different way.
fn osc_up_recovering(checkout_dir: &Path, emit: Emit<'_>) -> Result<Captured, Error> {
    let out = proc::capture("osc", &["up"], checkout_dir, emit)?;
    if out.success() {
        return Ok(out);
    }

    if is_inconsistent_wc_error(&out.stderr) {
        repair_wc(checkout_dir, emit)?;
        return Ok(proc::capture("osc", &["up"], checkout_dir, emit)?);
    }

    if let Some(name) = changes_conflict_file(&out.stderr) {
        // osc only raises this when `name` is untracked in this working
        // copy's `.osc/_files` (see `Package.__update`'s check against
        // `filenamelist_unvers`), so it is always safe to move aside — it
        // was never osc's file to begin with. The usual cause here is our
        // own `create_initial_changes` writing `<pkg>.changes` directly
        // before that revision's commit reached the server, and the
        // server later gaining its own file of the same name in the
        // meantime (e.g. a prior interrupted run that got further).
        let name = name.to_string();
        let conflicting = checkout_dir.join(&name);
        // This checkout can legitimately go through this recovery more than
        // once across retried pipeline attempts — `fs::rename` overwrites
        // silently on Unix, so without the existence check a second recovery
        // would clobber whatever the first one had moved aside with no trace
        // either backup ever existed.
        let mut backup = checkout_dir.join(format!("{name}.preexisting"));
        let mut n = 1;
        while backup.exists() {
            n += 1;
            backup = checkout_dir.join(format!("{name}.preexisting.{n}"));
        }
        let _ = std::fs::rename(&conflicting, &backup);
        let backup_name = backup.file_name().map(|n| n.to_string_lossy().into_owned());
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!(
                "osc up: untracked local '{name}' conflicted with a newer revision's \
                 file of the same name; moved it aside as '{}' and retrying",
                backup_name.as_deref().unwrap_or("<unknown>")
            ),
        });
        return Ok(proc::capture("osc", &["up"], checkout_dir, emit)?);
    }

    Ok(out)
}

/// Extract the filename osc names in its uncaught `PackageFileConflict`
/// message from `Package.__update()` (`osc/obs_scm/package.py`): `failed to
/// add file '<name>' file/dir with the same name already exists`. Raised by
/// `osc up` when a file of that name sits on disk but untracked in this
/// working copy while the server's newer revision also adds a file with the
/// same name — an unhandled exception in osc itself, not one of its usual
/// "inconsistent working copy" messages, so `repairwc` does not apply here.
fn changes_conflict_file(stderr: &str) -> Option<&str> {
    stderr
        .split("failed to add file '")
        .nth(1)?
        .split('\'')
        .next()
}

#[cfg(test)]
mod changes_conflict_tests {
    use super::*;

    #[test]
    fn extracts_the_filename_from_oscs_package_file_conflict_message() {
        let stderr = "osc: failed to add file 'python-foo.changes' \
                       file/dir with the same name already exists\n";
        assert_eq!(changes_conflict_file(stderr), Some("python-foo.changes"));
    }

    #[test]
    fn returns_none_for_unrelated_errors() {
        assert_eq!(changes_conflict_file("404 Not Found"), None);
    }
}

/// Return true if `checkout_dir`'s working copy has uncommitted local
/// changes (e.g. a spec version bump from a run that crashed before `osc
/// commit`). `osc status` prints one line per changed file and nothing when
/// the working copy is clean.
pub fn has_local_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    let Ok(out) = proc::capture("osc", &["status"], checkout_dir, emit) else {
        return true;
    };
    if out.success() {
        return !out.stdout.trim().is_empty();
    }
    if is_inconsistent_wc_error(&out.stderr) && repair_wc(checkout_dir, emit).is_ok() {
        return proc::capture("osc", &["status"], checkout_dir, emit)
            .map(|out| !out.stdout.trim().is_empty())
            .unwrap_or(true);
    }
    // `osc status` failed for a reason other than the one recognized,
    // repairable case (a network/server hiccup, say), or `repair_wc` itself
    // failed — either way `out.stdout` is from a failed invocation and
    // almost certainly empty, which used to fall through to the same
    // "no local changes" read as an actually-clean checkout. The caller
    // uses this to gate skipping a rebuild (see the `AlreadyBuilt` fast path
    // in pipeline.rs), so treat "couldn't tell" as "has changes" — silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    true
}

/// Whether an `osc` command failed because the working copy's `.osc/`
/// metadata is inconsistent — the message `osc` itself prints when a prior
/// run died mid-`up`/`ci` and left it half-written, naming `osc repairwc .`
/// as the fix.
fn is_inconsistent_wc_error(stderr: &str) -> bool {
    stderr.contains("inconsistent state") || stderr.contains("repairwc")
}

/// Run `osc repairwc .` to fix a working copy `osc` itself reports as
/// inconsistent (see [`is_inconsistent_wc_error`]). Note this can remove
/// files from `.osc/` — exactly what `osc`'s own error message warns about —
/// so it is only ever called after that specific error, not speculatively.
pub fn repair_wc(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["repairwc", "."], checkout_dir, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: "repairwc".to_string(),
            code: out.code,
            stderr: out.stderr,
        });
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
///
/// **Caveat (flagged by the 2026-10-06 external review):** `rpmspec` expands
/// macros using the **host** RPM configuration, not the target build repo's
/// configuration. A spec that `rpmspec --parse`s cleanly on this machine can
/// still fail to parse (or expand to different Requires / BuildRequires) on
/// the OBS build worker if the host's macro set diverges from the target
/// distro's — `%python3_sitelib`, `%python_module`, `%suse_version` and the
/// like are all host-local here. The agent still relies on this as a cheap
/// pre-flight syntax check; the one-shot [`emit_host_macro_warning`] call
/// below surfaces the gap to the user. A durable fix would require
/// containerized validation against the target repo's rpm-config.
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    emit_host_macro_warning(emit);

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
/// Reads `/build/<project>/_result` (XML) and interprets it with
/// [`obs_status::resolve`]: a bare `finished` is resolved through its
/// `<details>` verdict, and a terminal result in a `dirty` repository is only
/// trusted once the job history confirms it was built from the source
/// revision currently on the server (otherwise `Unknown("stale: ...")`).
///
/// If the API call or its parse fails, falls back to scraping `osc results`
/// so a surprising response shape degrades to the old behaviour instead of
/// breaking the build loop.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    if let Some(status) = get_build_status_xml(project, pkg_name, repo, arch) {
        return Ok(status);
    }

    let out = proc::capture_with_timeout(
        "osc",
        &["results", "-r", repo, "-a", arch, project, pkg_name],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("results -r {repo} -a {arch} {project} {pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_single_build_status(&out.stdout))
}

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// `osc api <path>` stdout, or `None` on any failure or timeout.
fn osc_api(path: &str) -> Option<String> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
    .ok()?;
    out.success().then_some(out.stdout)
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}

fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<BuildStatus> {
    let xml = osc_api(&format!(
        "/build/{project}/_result?package={}&repository={}&arch={}",
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    let entry = obs_status::find_entry(&xml, repo, arch, pkg_name)?;
    let evidence = if obs_status::needs_evidence(&entry) {
        obs_status::Evidence {
            source: osc_api(&format!("/source/{project}/{}?view=info", enc(pkg_name)))
                .and_then(|x| obs_status::parse_source_info(&x)),
            job: osc_api(&format!(
                "/build/{project}/{}/{}/_jobhistory?package={}&limit=1",
                enc(repo),
                enc(arch),
                enc(pkg_name)
            ))
            .and_then(|x| obs_status::parse_latest_job(&x)),
        }
    } else {
        obs_status::Evidence::default()
    };
    Some(obs_status::resolve(&entry, &evidence))
}

/// Parsing half of [`get_build_status`], split out so it is testable without
/// shelling out to `osc`.
fn parse_single_build_status(stdout: &str) -> BuildStatus {
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let raw = parts[parts.len() - 1];
            let word = raw.trim_end_matches('*');
            // OBS suffixes the status word with `*` when this result is for
            // an older source revision than what's on the server now (e.g. a
            // revision was committed while this result was already showing,
            // or right after). Reporting the stripped word as plain
            // `Succeeded` would make the `AlreadyBuilt` fast path in
            // pipeline.rs believe a since-superseded result still verifies
            // the current revision, and skip a rebuild that result was never
            // actually for — `has_local_changes` and `spec_version_differs`
            // both check the local checkout, so neither one catches "the
            // server has a newer committed revision than this result
            // covers". Reporting `Unknown` here forces that fast path to
            // fall through and resync instead of trusting a stale verdict.
            if raw != word {
                return BuildStatus::Unknown(format!("stale: {word}"));
            }
            return BuildStatus::from(word);
        }
    }
    BuildStatus::Unknown("no results".to_string())
}

#[cfg(test)]
mod single_build_status_tests {
    use super::*;

    #[test]
    fn a_starred_status_is_unknown_not_its_unstarred_meaning() {
        // The real incident this guards against: committing a new revision
        // and immediately watching build status used to report `Succeeded`
        // from the *previous* revision's still-displayed, now-stale result,
        // before OBS had even started rebuilding the new one.
        let stdout = "python-foo  openSUSE_Tumbleweed  x86_64  succeeded*\n";
        assert_eq!(
            parse_single_build_status(stdout),
            BuildStatus::Unknown("stale: succeeded".to_string())
        );
    }

    #[test]
    fn an_unstarred_status_is_read_normally() {
        let stdout = "python-foo  openSUSE_Tumbleweed  x86_64  succeeded\n";
        assert_eq!(parse_single_build_status(stdout), BuildStatus::Succeeded);
    }
}

/// Query the OBS build status of every package in `project` for one
/// repo/arch in a single call.
///
/// Runs `osc results -r <repo> -a <arch> <project>` (no package name lists
/// every package). Unlike the single-package form, this layout is NOT
/// `<pkg> ... <status-word>` per line: `osc` prints the package names as an
/// indented tree, one per line, wrapping into as many same-order blocks as
/// fit the terminal width, and after each block a single status line packs
/// one letter code per package into fixed two-column slots (`<code><sep>`),
/// e.g. `. B U U . U ...  openSUSE_Tumbleweed x86_64 (published)`. A blank
/// (space) code means "disabled" — indistinguishable from the column
/// separator under naive whitespace splitting — hence the positional
/// indexing here instead of `split_whitespace`.
pub fn get_project_build_statuses(
    project: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> HashMap<String, BuildStatus> {
    // XML first: one entry per package with a named code, instead of the
    // positional letter table below (which depends on terminal width and
    // `osc`'s padding). Fall back to the table if the API path yields nothing.
    if let Some(xml) = osc_api(&format!(
        "/build/{project}/_result?repository={repo}&arch={arch}"
    )) {
        let statuses: HashMap<String, BuildStatus> = obs_status::parse_results(&xml)
            .into_iter()
            .filter(|e| e.repo == repo && e.arch == arch)
            .map(|e| (e.pkg.to_lowercase(), obs_status::resolve_lenient(&e)))
            .collect();
        if !statuses.is_empty() {
            return statuses;
        }
    }

    let Ok(out) = proc::capture(
        "osc",
        &["results", "-r", repo, "-a", arch, project],
        &anywhere(),
        emit,
    ) else {
        return HashMap::new();
    };
    if !out.success() {
        return HashMap::new();
    }

    parse_project_build_statuses(&out.stdout, repo, arch)
}

/// Parsing half of [`get_project_build_statuses`], split out so it is
/// testable without shelling out to `osc`.
fn parse_project_build_statuses(
    stdout: &str,
    repo: &str,
    arch: &str,
) -> HashMap<String, BuildStatus> {
    let marker = format!(" {repo} {arch} (");
    let mut statuses = HashMap::new();
    let mut pending_pkgs: Vec<String> = Vec::new();

    for line in stdout.lines() {
        if line.trim() == "Legend:" {
            break;
        }
        if let Some(marker_idx) = line.find(&marker) {
            let chars: Vec<char> = line.chars().collect();
            let n = pending_pkgs.len();
            // The code table is dense two-char slots (`<code><sep>`), but
            // real `osc` builds disagree on whether there's a leading sep
            // before the first code and a trailing one after the last: some
            // print it flush (`code sep code ... code`, length `2n-1`, codes
            // at `2*i`), others pad both ends (`sep code sep ... code sep`,
            // length `2n+1`, codes at `1 + 2*i`). Derive the offset from the
            // actual prefix length instead of hardcoding one — hardcoding
            // `2*i` silently read every code as its own separator space
            // against the padded form, making every package look
            // "disabled" and so falsely resolvable.
            let prefix_len = line[..marker_idx].chars().count() as isize;
            let offset = if n > 0 {
                ((prefix_len - 2 * n as isize + 1) / 2).max(0) as usize
            } else {
                0
            };
            for (i, pkg) in pending_pkgs.iter().enumerate() {
                if let Some(&c) = chars.get(offset + 2 * i)
                    && c != ' '
                {
                    statuses.insert(pkg.clone(), BuildStatus::from_code(c));
                }
            }
            pending_pkgs.clear();
            continue;
        }
        let name = line.trim_start_matches([' ', '|']).trim();
        if !name.is_empty() {
            pending_pkgs.push(name.to_lowercase());
        }
    }

    statuses
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
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting RPMs written in the last few minutes.
fn downloaded_rpm_count(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &std::path::Path,
) -> usize {
    let listing = osc_api(&format!(
        "/build/{project}/{}/{}/{}",
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml));
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_recent_rpms(rpms_dir, Duration::from_secs(600)),
    }
}

/// `.rpm` files (not `.src.rpm`) in `rpms_dir` modified within `window`.
/// Returns 0 when the directory doesn't exist or can't be read.
fn count_recent_rpms(rpms_dir: &std::path::Path, window: Duration) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    iter.filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".rpm") && !n.ends_with(".src.rpm")
        })
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age <= window)
        })
        .count()
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

/// Try to extract unresolvable package names (and their raw version-detail
/// lines, see [`extract_unresolvable_detail`]) from `osc results --verbose`.
///
/// Used as a fallback when the build log doesn't contain "nothing provides" lines.
fn get_unresolvable_from_results(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    let Ok(out) = proc::capture(
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
    ) else {
        return (vec![], vec![]);
    };
    (
        extract_unresolvable(&out.stdout),
        extract_unresolvable_detail(&out.stdout),
    )
}

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: reads whatever
/// build log already sits at `log_path`, falling back to `osc results
/// --verbose` when that log has no "nothing provides" lines (the same
/// fallback [`obs_build`] uses for a build it just ran). Exposed so a caller
/// that finds a package already `Unresolvable` on restart can inform the fix
/// agent immediately instead of spending an attempt rediscovering it through
/// a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    let log_text = fs::read_to_string(log_path).unwrap_or_default();
    let mut names = extract_unresolvable(&log_text);
    let mut detail = extract_unresolvable_detail(&log_text);
    if names.is_empty() {
        (names, detail) = get_unresolvable_from_results(project, pkg_name, repo, arch, emit);
    }
    (names, detail)
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

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;
    const STALE_PATIENCE: Duration = Duration::from_secs(600);
    let mut stale_since: Option<Instant> = None;

    let status = loop {
        if start.elapsed() >= MAX_WAIT {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s waiting for OBS build (last status: {})",
                    MAX_WAIT.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status(project, pkg_name, repo, arch, Emit::quiet()) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(e);
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
        };
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

        // `stale: <verdict>` means "a terminal result I couldn't confirm is
        // for the current sources". Normally the scheduler catches up within
        // a minute or two; if it stays like that this long the repo is stuck
        // dirty, so take the verdict at face value instead of idling until
        // the overall timeout.
        if let BuildStatus::Unknown(s) = &status
            && let Some(word) = s.strip_prefix("stale: ")
        {
            let since = *stale_since.get_or_insert_with(Instant::now);
            let verdict = BuildStatus::from(word);
            if since.elapsed() >= STALE_PATIENCE && verdict.is_terminal() {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS result has been unconfirmed for {}s (repository stuck dirty?); \
                         taking its verdict, {verdict}, at face value",
                        STALE_PATIENCE.as_secs()
                    ),
                });
                break verdict;
            }
        } else {
            stale_since = None;
        }

        std::thread::sleep(POLL_INTERVAL);
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    let _ = fetch_buildlog(project, pkg_name, repo, arch, log_path, Emit::quiet());
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. The Oct 6 review's reproduction of
        // 'succeeded status followed by a failing getbinaries still
        // returns success, leaving the RPM directory empty' turned on
        // this call being non-fatal: a succeeded OBS build without any
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
        let fetch_result = get_binaries(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = downloaded_rpm_count(project, pkg_name, repo, arch, rpms_dir);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded but downloading the built RPMs \
                 failed: {e}. A green source without downloadable artifacts is not \
                 a usable build — a dependent package would see every \
                 BuildRequires as unresolvable. Re-check the OBS project's binary \
                 publishing config for repo={repo}/arch={arch}."
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

    let log_text = fs::read_to_string(log_path).unwrap_or_default();

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) =
                current_unresolvable(project, pkg_name, repo, arch, log_path, emit);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken => Error::NotActionable("OBS: package source is broken".to_string()),
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
            FailureKind::NotWorkingCopy => {
                Error::NotActionable("osc checkout is not server-backed".to_string())
            }
            FailureKind::UnresolvableDeps { names, detail } => {
                Error::UnresolvableDeps { names, detail }
            }
            FailureKind::BuildFailed => Error::BuildFailed {
                spec: format!("{pkg_name}.spec"),
                code: 1,
                log: log_path.to_owned(),
            },
        },
    })
}

#[cfg(test)]
mod count_rpms_tests {
    use super::*;

    #[test]
    fn recent_rpms_exclude_source_rpms_and_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("python313-Events-0.5-2.1.noarch.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("python-events-0.5-2.1.src.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("_buildenv"), b"").unwrap();
        assert_eq!(count_recent_rpms(tmp.path(), Duration::from_secs(60)), 1);
    }

    #[test]
    fn recent_rpms_tolerates_missing_dir() {
        assert_eq!(
            count_recent_rpms(
                std::path::Path::new("/does/not/exist"),
                Duration::from_secs(60)
            ),
            0
        );
    }
}

#[cfg(test)]
mod project_build_statuses_tests {
    use super::*;

    /// Regression test for the off-by-one that used to read the separator
    /// space between status codes instead of the code itself, silently
    /// returning no statuses at all and making every OBS-known package look
    /// "resolvable" regardless of its actual build state.
    #[test]
    fn reads_status_codes_at_even_columns_not_the_separators() {
        let stdout = "\
python-foo
python-bar
python-baz
. B U openSUSE_Tumbleweed x86_64 (published)
Legend:
 . succeeded
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    /// Regression test for the padded real-world format seen from actual
    /// `osc results` output on a project with many packages: a leading
    /// separator before the first code and a trailing one after the last
    /// (`" . . F ...  openSUSE_Tumbleweed x86_64 (published)"`, note the
    /// double space before the repo name), as opposed to the flush format
    /// the other tests here use. Both must parse to the same codes.
    #[test]
    fn reads_status_codes_in_the_padded_real_world_format() {
        let stdout = "\
python-foo
python-bar
python-baz
 . B U  openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    #[test]
    fn a_space_code_is_treated_as_no_status_rather_than_disabled() {
        // The blank/disabled code is indistinguishable from the column
        // separator, so it must be skipped rather than recorded — a caller
        // treating "no entry" as "not resolvable" is the safe default.
        let stdout = "\
python-foo
python-bar
.   openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), None);
    }

    #[test]
    fn stops_parsing_at_the_legend_footer() {
        let stdout = "\
python-foo
F openSUSE_Tumbleweed x86_64 (published)
Legend:
F failed
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");
        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Failed));
        assert_eq!(statuses.len(), 1);
    }
}

#[cfg(test)]
mod project_repo_paths_tests {
    use super::*;

    #[test]
    fn extracts_project_and_repository_from_each_path() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
    <arch>x86_64</arch>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn deduplicates_the_same_pair_across_repositories() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
  <repository name=\"openSUSE_Tumbleweed_i586\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn has_repository_true_when_one_is_present() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        assert!(parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_on_empty_project() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <title>empty</title>
  <description/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_when_only_devel_project_present() {
        // A `<devel project="..."/>` element is not a build repository and
        // must not be mistaken for one.
        let xml = "\
<project name=\"home:dspinella:python\">
  <devel project=\"devel:languages:python\"/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn a_devel_project_attribute_elsewhere_is_not_mistaken_for_a_path() {
        // `<devel project="..."/>` uses the same `project="..."` attribute
        // shape but is not a build path and has no `repository` attribute.
        let xml = "\
<project name=\"python-foo\">
  <devel project=\"devel:languages:python\"/>
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }
}
