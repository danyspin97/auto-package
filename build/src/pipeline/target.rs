//! Target selection: the distro bound plan, the explicit target, the claims, the
//! system tool aliases and the names read from requires_dist.

use super::*;

// ── target selection ─────────────────────────────────────────────────────────

/// The distro-bound check of `select_target`: the versions the linked projects
/// ship for the distro nodes a bound is declared on, looked up in parallel
/// (each is an OBS round trip), then [`distro_bounds::plan`]. A version that
/// cannot be read is left out, which reports nothing for it.
pub(super) fn distro_bound_plan(
    project: &str,
    nodes: &[ap_ecosystem::pip::Node],
    distro: &[bool],
    needs_build: &[bool],
    linked: &[(String, std::sync::Arc<std::collections::HashSet<String>>)],
) -> distro_bounds::Plan {
    let candidates = distro_bounds::candidates(nodes, distro, needs_build);
    // The requested package, when the distro ships it: its version is checked
    // against the resolved one rather than against a bound.
    let root = nodes
        .iter()
        .position(|n| n.is_requested)
        .filter(|&i| distro[i]);
    let mut to_fetch = candidates.clone();
    to_fetch.extend(root.filter(|r| !candidates.contains(r)));
    if to_fetch.is_empty() {
        return distro_bounds::Plan::default();
    }
    // The linked projects nearest first, each by fuzzy key: the version is the
    // first one that ships the package and has a source version for it.
    let by_project: Vec<(&str, std::collections::HashMap<String, &String>)> = linked
        .iter()
        .map(|(path_project, listing)| {
            let by_key = listing.iter().map(|p| (osc::fuzzy_key(p), p)).collect();
            (path_project.as_str(), by_key)
        })
        .collect();
    let lookup = |i: usize| -> Option<String> {
        let key = osc::fuzzy_key(&osc::rpm_name(&nodes[i].name));
        by_project.iter().find_map(|(path_project, by_key)| {
            let real = by_key.get(&key)?;
            osc::source_version(path_project, real)
        })
    };
    let fetched = distro_bounds::fetch_parallel(&to_fetch, 8, lookup);
    // A node found only once an earlier conflict turned a distro node into one
    // built here was not prefetched; the few of those are looked up singly.
    let tried: std::collections::HashSet<usize> = to_fetch.iter().copied().collect();
    let mut plan = distro_bounds::plan(nodes, distro, needs_build, |i| {
        if tried.contains(&i) {
            fetched.get(&i).cloned()
        } else {
            lookup(i)
        }
    });
    if let Some(r) = root
        && let Some(shipped) = fetched.get(&r)
    {
        plan.notes.extend(distro_bounds::stale_root_message(
            &nodes[r], shipped, project,
        ));
    }
    plan
}

pub struct TargetInfo {
    /// PyPI display name (as returned by the API, e.g. `MarkupSafe`).
    pub name: String,
    pub version: String,
    pub reason: SelectionReason,
    pub blocked_by: Vec<String>,
}

/// Find the node `--target` names in the resolved closure.
///
/// Matches against both the node's own (PyPI) name and its derived RPM name,
/// fuzzily — `-`/`_`/`.` and case are not meaningful dependency identity (see
/// `fuzzy_key`'s doc comment), and a caller has no way to know in advance
/// which of the two spellings a given node happens to be keyed under. Getting
/// this wrong would silently check out and commit to a brand-new, wrongly-named
/// OBS package (`python-backports.zstd` for `python-backports-zstd`).
pub(super) fn find_explicit_target<'a>(
    nodes: &'a [ap_ecosystem::pip::Node],
    target: &str,
) -> Result<&'a ap_ecosystem::pip::Node, String> {
    let target_key = osc::fuzzy_key(target);
    nodes
        .iter()
        .find(|n| {
            osc::fuzzy_key(&n.name) == target_key
                || osc::fuzzy_key(&osc::rpm_name(&n.name)) == target_key
        })
        .ok_or_else(|| {
            let mut names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            names.sort_unstable();
            format!(
                "package '{target}' not in dependency closure — available: {}",
                names.join(", ")
            )
        })
}

/// Does OBS reporting this status mean a dependent package will actually find
/// a built binary when it resolves against this one?
///
/// Only `Succeeded` is a hard yes; `Blocked`/`Building`/`Scheduled` are "will
/// be, soon" and left in to avoid duplicate scheduling of work OBS is already
/// doing. Everything else — `Failed`, `Unresolvable`, `Broken`, `Disabled`,
/// `Excluded`, `Unknown`, or `None` (no entry at all for this distro/arch) —
/// is a no.
///
/// Only a built status counts as resolvable. A Disabled source or a package
/// never built for this arch has no binaries in the repo, so reading it as
/// resolvable would leave nothing to build and let the run exit successfully.
pub fn status_is_resolvable(status: Option<&osc::BuildStatus>) -> bool {
    matches!(
        status,
        Some(
            osc::BuildStatus::Succeeded
                | osc::BuildStatus::Blocked
                | osc::BuildStatus::Building
                | osc::BuildStatus::Scheduled
        )
    ) || matches!(status, Some(osc::BuildStatus::Unknown(code)) if code == "finished")
}

/// Tell the user, once per run, when pip does not run in its sandbox: either
/// no sandbox works here and sdists are skipped, or `--unsafe-resolve` turned
/// the sandbox off.
pub(super) fn warn_unsandboxed_resolve_once(reporter: &dyn Reporter) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        if let Some(message) = ap_ecosystem::pip::resolve_mode().notice() {
            reporter.report(&Event::Warning {
                step: None,
                message,
            });
        }
    });
}

/// What each root of this process resolved a package to, per OBS project:
/// `(project, fuzzy_key) -> [(root, version)]`.
type Claims = std::collections::HashMap<(String, String), Vec<(String, String)>>;

pub(super) fn claims() -> &'static std::sync::Mutex<(Claims, std::collections::HashSet<String>)> {
    static CLAIMS: std::sync::OnceLock<
        std::sync::Mutex<(Claims, std::collections::HashSet<String>)>,
    > = std::sync::OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

/// PyPI names (normalized, see [`normalize_pkg_name`]) that are pure
/// redistributions of a tool openSUSE already ships as a system package of
/// the same name. Callers depending on one of these want the executable on
/// PATH, not an importable Python module — see the `resolvable` computation
/// in [`select_target`] and convention 27's "PyPI packages that just
/// redistribute a system tool" in the agent prompt.
///
/// Deliberately a single, hardcoded, hand-verified entry rather than a
/// user-extensible config file: unlike `flavors.toml` (which genuinely
/// varies per work directory/distro), whether a PyPI package just
/// redistributes a system binary is a fixed fact about that package, not
/// something that differs between work directories — and confirming one
/// needs checking that the actual consumer shells out to the tool rather
/// than importing its Python module (`ninja` is correct because that one
/// case was spot-checked, not because every same-shaped PyPI package works
/// this way). That verification belongs in reviewed source control, not a
/// `<work-dir>/`-local file that `--cleanup` can wipe and that never
/// benefits any other work directory in the first place.
pub(super) const SYSTEM_TOOL_ALIASES: &[&str] = &["ninja"];

/// Whether `name` (a bare PyPI/import name, not an RPM name) is one of
/// [`SYSTEM_TOOL_ALIASES`] — a dependency that should resolve against the
/// existing system package instead of ever being built as its own
/// `python-<name>` RPM. Exposed so callers outside this module (the CLI's
/// agent-discovered-dependency queue) can skip these the same way
/// `select_target` does for pip-closure nodes.
pub fn is_system_tool_alias(name: &str) -> bool {
    SYSTEM_TOOL_ALIASES.contains(&normalize_pkg_name(name).as_str())
}

/// Normalize a PyPI package name to a consistent lookup key.
///
/// Delegates to [`osc::fuzzy_key`] rather than keeping a second, weaker
/// definition here: lowercasing and folding `-` into `_` alone would not match
/// a dotted name (`ruamel.yaml`) against a `requires_dist` entry spelling it
/// with a hyphen or underscore. The same class of mismatch `fuzzy_key`'s doc
/// comment documents for RPM names applies on the PyPI-graph side too, instead
/// of the OBS
/// side.
pub(super) fn normalize_pkg_name(name: &str) -> String {
    osc::fuzzy_key(name)
}

/// Extract bare package names from `requires_dist` lines.
///
/// Skips extras-only lines (`; extra == ...`). For all other lines, the name
/// is the portion before any version specifier, extras bracket, env marker,
/// or whitespace.
pub(super) fn dep_names_from_requires_dist(requires_dist: &[String]) -> Vec<String> {
    requires_dist
        .iter()
        .filter_map(|r| {
            // Skip optional (extras-gated) deps.
            if r.contains("extra ==") || r.contains("extra==") {
                return None;
            }
            let name = r
                .split(|c: char| {
                    c == '('
                        || c == '['
                        || c == ';'
                        || c == '>'
                        || c == '<'
                        || c == '='
                        || c == '!'
                        || c == ' '
                })
                .next()?
                .trim();
            if name.is_empty() {
                None
            } else {
                Some(normalize_pkg_name(name))
            }
        })
        .collect()
}
