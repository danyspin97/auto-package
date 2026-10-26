//! Does the version a linked project (openSUSE:Factory) ships meet the bound a
//! package that has to be built here declares on it?
//!
//! `select_target` counts a package present in a linked project as resolved
//! without looking at its version. That is right until a package built here
//! declares a bound the shipped version misses: `tilelang` needs
//! `apache-tvm-ffi <=0.1.11` while Factory ships `0.1.14.post1`, `open-webui`
//! pins `redis==8.0.1` against Factory's `7.0.1`. OBS then fails the dependent
//! with `nothing provides ...` and the failure reaches the agent as a missing
//! package, long after the cause was knowable from the closure.
//!
//! The two directions need different handling:
//!
//! - The distro's version is too NEW (above an upper bound or an exact pin):
//!   upstream's bound is usually only the version it was tested with, so the
//!   dependent's spec relaxes it (convention 59), and only a real API break
//!   makes the distro package worth branching and patching (convention 62).
//!   The dependency stays resolved; the conflict is reported.
//! - The distro's version is too OLD (below a lower bound or an exact pin):
//!   relaxing cannot help, so the distro's package is branched into this
//!   project and updated to the version pip resolved, carrying the distro's own
//!   recipe forward (not packaged from scratch), where it shadows the distro's
//!   package. The dependency is no longer resolved.
//!
//! Only dependents that still have to be built here are checked: a package the
//! distro ships was built against what the distro ships.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use ap_ecosystem::pip::Node;
use pep440_rs::{Operator, Version, VersionSpecifiers};

use crate::osc::fuzzy_key;

/// How a version that misses a bound misses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Above an upper bound or an exact pin.
    DistroTooNew,
    /// Below a lower bound or an exact pin.
    DistroTooOld,
}

fn parse_version(text: &str) -> Option<Version> {
    Version::from_str(text.trim()).ok()
}

/// `>=1.2,<2` (or `(>=1.2, <2)`) as a bound. `None` when it is not understood,
/// so a bound this cannot read is never held against a version.
pub(crate) fn parse_specifier(text: &str) -> Option<VersionSpecifiers> {
    let text = text.trim().trim_start_matches('(').trim_end_matches(')');
    VersionSpecifiers::from_str(text).ok()
}

/// Which way `version` misses `bound`, or `None` when it meets it.
fn violation(bound: &VersionSpecifiers, version: &Version) -> Option<Kind> {
    let failed = bound.iter().find(|s| !s.contains(version))?;
    // `~=X` is two-sided (`>=X` and `==X.*`): a version below `X` is too old,
    // one at or above the next prefix bump is too new.
    Some(match failed.operator() {
        Operator::GreaterThan | Operator::GreaterThanEqual => Kind::DistroTooOld,
        Operator::TildeEqual
        | Operator::Equal
        | Operator::EqualStar
        | Operator::ExactEqual
            if version < failed.version() =>
        {
            Kind::DistroTooOld
        }
        _ => Kind::DistroTooNew,
    })
}

/// One `Requires-Dist` line: the name, the version bound (empty when there is
/// none) and the environment marker.
fn parse_requirement(line: &str) -> Option<(String, String, Option<String>)> {
    let (head, marker) = match line.split_once(';') {
        Some((h, m)) => (h, Some(m.trim().to_string())),
        None => (line, None),
    };
    let head = head.trim();
    let end = head
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(head.len());
    let name = &head[..end];
    if name.is_empty() {
        return None;
    }
    let mut rest = head[end..].trim();
    if rest.starts_with('[') {
        rest = rest[rest.find(']')? + 1..].trim();
    }
    // A direct reference (`name @ url`) carries no version bound.
    let spec = if rest.starts_with('@') { "" } else { rest };
    Some((name.to_string(), spec.to_string(), marker))
}

/// Whether a marker can hold on the Linux x86_64 CPython this builds for. Only
/// the platform variables are evaluated, in a flat `and`/`or` expression; a
/// marker this does not understand, or one on the Python version, is taken to
/// apply, so a bound is checked rather than skipped. An `extra == ...` marker
/// is an optional dependency and does not apply.
fn marker_applies(marker: &str) -> bool {
    if marker.contains("extra") {
        return false;
    }
    if marker.contains('(') {
        return true;
    }
    marker
        .split(" or ")
        .any(|conj| conj.split(" and ").all(clause_applies))
}

fn clause_applies(clause: &str) -> bool {
    let clause = clause.trim();
    let Some((op, at)) = ["==", "!="]
        .iter()
        .find_map(|op| clause.find(op).map(|at| (*op, at)))
    else {
        return true;
    };
    let var = clause[..at].trim();
    let value = clause[at + 2..].trim().trim_matches(['"', '\'']);
    let ours = match var {
        "sys_platform" => "linux",
        "platform_system" => "Linux",
        "platform_machine" => "x86_64",
        "os_name" => "posix",
        "platform_python_implementation" => "CPython",
        "implementation_name" => "cpython",
        _ => return true,
    };
    (ours == value) == (op == "==")
}

/// How `shipped` (a version a linked project has) misses the bound that
/// `requires_dist` (a package's `Requires-Dist` lines) declares on `dependency`,
/// with the bound as written; `None` when it meets it, there is no bound, or
/// the bound only applies to an extra or another platform.
pub fn bound_violation(
    requires_dist: &[String],
    dependency: &str,
    shipped: &str,
) -> Option<(Kind, String)> {
    let version = parse_version(shipped)?;
    let wanted = fuzzy_key(dependency);
    for line in requires_dist {
        let Some((name, spec, marker)) = parse_requirement(line) else {
            continue;
        };
        if fuzzy_key(&name) != wanted
            || spec.is_empty()
            || crate::redist::component(&name).is_some()
        {
            continue;
        }
        if marker.as_deref().is_some_and(|m| !marker_applies(m)) {
            continue;
        }
        let Some(bound) = parse_specifier(&spec) else {
            continue;
        };
        if let Some(kind) = violation(&bound, &version) {
            return Some((kind, spec));
        }
    }
    None
}

/// A dependency whose distro version misses a bound a dependent declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundConflict {
    /// The dependency's PyPI name.
    pub dependency: String,
    /// The version the linked project ships.
    pub distro_version: String,
    /// The version pip resolved, which meets every bound in the closure.
    pub resolved_version: String,
    /// The dependent's PyPI name.
    pub dependent: String,
    /// The bound the distro's version misses.
    pub specifier: String,
    pub kind: Kind,
}

impl BoundConflict {
    /// The handling this conflict gets, as a sentence for the run log.
    pub fn message(&self, project: &str) -> String {
        let head = format!(
            "{} {} (shipped by a project {project} links to) misses the bound `{}` that {} declares",
            self.dependency, self.distro_version, self.specifier, self.dependent
        );
        match self.kind {
            Kind::DistroTooNew => format!(
                "{head}: relax the bound in {}'s spec (convention 59); only a real API break \
                 makes patching the distro package worth it (convention 62)",
                self.dependent
            ),
            Kind::DistroTooOld => format!(
                "{head}: relaxing cannot help, so {} {} is built in this project instead",
                self.dependency, self.resolved_version
            ),
        }
    }
}

/// The bound conflicts and notes of this run already written to the log.
/// `select_target` runs every round and re-derives the same plan, so without
/// this open-webui's forty `==` pins are logged forty times a round. A conflict
/// is told once per (project, dependent, dependency); a note once per text.
#[derive(Debug, Default)]
pub struct Reported(HashSet<String>);

impl Reported {
    /// Whether `conflict` was not reported yet; marks it reported.
    pub fn first_conflict(&mut self, project: &str, conflict: &BoundConflict) -> bool {
        self.0.insert(format!(
            "conflict\0{project}\0{}\0{}",
            fuzzy_key(&conflict.dependent),
            fuzzy_key(&conflict.dependency)
        ))
    }

    /// Whether `note` was not reported yet; marks it reported.
    pub fn first_note(&mut self, project: &str, note: &str) -> bool {
        self.0.insert(format!("note\0{project}\0{note}"))
    }
}

/// What this process has reported (one process is one run).
pub fn reported() -> std::sync::MutexGuard<'static, Reported> {
    static REPORTED: std::sync::OnceLock<std::sync::Mutex<Reported>> = std::sync::OnceLock::new();
    REPORTED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// What the check found.
#[derive(Debug, Default)]
pub struct Plan {
    pub conflicts: Vec<BoundConflict>,
    /// Indices of distro nodes to build here after all (their distro version
    /// is too old for a dependent that is built here).
    pub rebuild: Vec<usize>,
    /// Other findings worth a line in the run log.
    pub notes: Vec<String>,
}

/// The nodes `distro` marks that a node in `needs_build` declares a bound on:
/// the ones whose shipped version is worth looking up.
pub fn candidates(nodes: &[Node], distro: &[bool], needs_build: &[bool]) -> Vec<usize> {
    let by_name = index_by_name(nodes);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if !needs_build[i] {
            continue;
        }
        for (j, _) in bounded_deps(node, &by_name) {
            if distro[j] && seen.insert(j) {
                out.push(j);
            }
        }
    }
    out
}

fn index_by_name(nodes: &[Node]) -> HashMap<String, usize> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (fuzzy_key(&n.name), i))
        .collect()
}

/// The closure nodes `node` declares a version bound on, with the bound text.
fn bounded_deps(node: &Node, by_name: &HashMap<String, usize>) -> Vec<(usize, String)> {
    node.requires_dist
        .iter()
        .filter_map(|line| {
            let (name, spec, marker) = parse_requirement(line)?;
            if spec.is_empty() || !marker.as_deref().is_none_or(marker_applies) {
                return None;
            }
            // A CUDA component's version is NVIDIA's, not the wheel's: a bound
            // on the wheel (`nvidia-cublas>=13.1`) says nothing about it.
            if crate::redist::component(&name).is_some() {
                return None;
            }
            Some((*by_name.get(&fuzzy_key(&name))?, spec))
        })
        .collect()
}

/// Check every bound a node that is built here declares on a node that comes
/// from the distro. `distro[i]` is true for a node the linked project supplies,
/// `needs_build[i]` for one that still has to be built here (the requested
/// root included). `distro_version(i)` is the version the linked project ships
/// for node `i`, or `None` when it cannot be told: a dependency whose version
/// is unknown, or whose bound cannot be parsed, is never reported.
///
/// A distro node that must be rebuilt here becomes a node that is built here,
/// so its own bounds are checked too, until nothing changes.
pub fn plan(
    nodes: &[Node],
    distro: &[bool],
    needs_build: &[bool],
    mut distro_version: impl FnMut(usize) -> Option<String>,
) -> Plan {
    let by_name = index_by_name(nodes);
    let mut distro = distro.to_vec();
    let mut needs_build = needs_build.to_vec();
    let mut versions: HashMap<usize, Option<String>> = HashMap::new();
    let mut plan = Plan::default();
    let mut reported: HashSet<(usize, usize)> = HashSet::new();
    loop {
        let mut changed = false;
        for i in 0..nodes.len() {
            if !needs_build[i] {
                continue;
            }
            for (j, spec) in bounded_deps(&nodes[i], &by_name) {
                if !distro[j] || reported.contains(&(i, j)) {
                    continue;
                }
                let Some(shipped) = versions
                    .entry(j)
                    .or_insert_with(|| distro_version(j))
                    .clone()
                else {
                    continue;
                };
                let (Some(clauses), Some(v)) = (parse_specifier(&spec), parse_version(&shipped))
                else {
                    continue;
                };
                let Some(kind) = violation(&clauses, &v) else {
                    continue;
                };
                reported.insert((i, j));
                plan.conflicts.push(BoundConflict {
                    dependency: nodes[j].name.clone(),
                    distro_version: shipped,
                    resolved_version: nodes[j].version.clone(),
                    dependent: nodes[i].name.clone(),
                    specifier: spec,
                    kind,
                });
                if kind == Kind::DistroTooOld {
                    distro[j] = false;
                    needs_build[j] = true;
                    plan.rebuild.push(j);
                    changed = true;
                }
            }
        }
        if !changed {
            return plan;
        }
    }
}

/// A warning for the requested package when the linked project ships an older
/// release than the one pip resolved: it counts as resolved by presence, so
/// without this the run would end having built everything but the package that
/// was asked for, with nothing saying why.
pub fn stale_root_message(node: &Node, shipped: &str, project: &str) -> Option<String> {
    let (shipped_v, resolved_v) = (parse_version(shipped)?, parse_version(&node.version)?);
    (shipped_v < resolved_v).then(|| {
        format!(
            "{} {shipped} (shipped by a project {project} links to) is older than the {} that was resolved, and counts \
             as available: nothing builds it here (pass --force to branch it into this project and update it to {})",
            node.name, node.version, node.version
        )
    })
}

/// `f(i)` for each of `indices`, `jobs` at a time, keeping the answers.
pub fn fetch_parallel(
    indices: &[usize],
    jobs: usize,
    f: impl Fn(usize) -> Option<String> + Sync,
) -> HashMap<usize, String> {
    let next = AtomicUsize::new(0);
    let found = std::sync::Mutex::new(HashMap::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1).min(indices.len()) {
            scope.spawn(|| {
                while let Some(&i) = indices.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(v) = f(i) {
                        found.lock().unwrap_or_else(|e| e.into_inner()).insert(i, v);
                    }
                }
            });
        }
    });
    found.into_inner().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse_version(s).unwrap_or_else(|| panic!("{s} does not parse"))
    }

    fn meets(version: &str, spec: &str) -> bool {
        let clauses = parse_specifier(spec).unwrap_or_else(|| panic!("{spec} does not parse"));
        violation(&clauses, &v(version)).is_none()
    }

    fn conflict(dependent: &str, dependency: &str) -> BoundConflict {
        BoundConflict {
            dependency: dependency.into(),
            distro_version: "2.13.5".into(),
            resolved_version: "2.13.4".into(),
            dependent: dependent.into(),
            specifier: "==2.13.4".into(),
            kind: Kind::DistroTooNew,
        }
    }

    /// The same plan is derived every round; each (dependent, dependency) is
    /// told once, however spelled, and notes once per text.
    #[test]
    fn a_conflict_is_reported_once_per_dependent_and_dependency() {
        let mut reported = Reported::default();
        assert!(reported.first_conflict("p:x", &conflict("open-webui", "pydantic")));
        assert!(!reported.first_conflict("p:x", &conflict("open-webui", "pydantic")));
        assert!(!reported.first_conflict(
            "p:x",
            &conflict("Open_WebUI".replace("WebUI", "webui").as_str(), "Pydantic")
        ));
        // Another dependency, another dependent, another project: new.
        assert!(reported.first_conflict("p:x", &conflict("open-webui", "redis")));
        assert!(reported.first_conflict("p:x", &conflict("vllm", "pydantic")));
        assert!(reported.first_conflict("p:y", &conflict("open-webui", "pydantic")));
        assert!(reported.first_note("p:x", "numpy 2 is older"));
        assert!(!reported.first_note("p:x", "numpy 2 is older"));
        assert!(reported.first_note("p:y", "numpy 2 is older"));
    }

    #[test]
    fn versions_order_as_pep_440() {
        assert!(v("1.0.dev1") < v("1.0a1"));
        assert!(v("1.0a1") < v("1.0b1"));
        assert!(v("1.0b1") < v("1.0rc1"));
        assert!(v("1.0rc1") < v("1.0"));
        assert!(v("1.0") < v("1.0.post1"));
        assert!(v("0.1.11") < v("0.1.14.post1"));
        assert!(v("0.1.14.post1") > v("0.1.14"));
        assert_eq!(v("1.0"), v("1.0.0"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert!(v("1!0.1") > v("5.0"));
        assert!(v("0.66b0") < v("0.66b1"));
        assert!(v("1.0-1") > v("1.0"));
        assert!(parse_version("not a version").is_none());
    }

    #[test]
    fn the_apache_tvm_ffi_case_is_a_too_new_distro_version() {
        let clauses = parse_specifier(">=0.1.10,<=0.1.11").unwrap();
        assert_eq!(
            violation(&clauses, &v("0.1.14.post1")),
            Some(Kind::DistroTooNew)
        );
        assert_eq!(violation(&clauses, &v("0.1.9")), Some(Kind::DistroTooOld));
        assert_eq!(violation(&clauses, &v("0.1.11")), None);
    }

    #[test]
    fn an_exact_pin_misses_in_either_direction() {
        let pin = parse_specifier("==8.0.1").unwrap();
        assert_eq!(violation(&pin, &v("7.0.1")), Some(Kind::DistroTooOld));
        assert_eq!(violation(&pin, &v("8.1.0")), Some(Kind::DistroTooNew));
        assert_eq!(violation(&pin, &v("8.0.1")), None);
        assert!(meets("1.2.7", "==1.2.*"));
        assert!(!meets("1.3.0", "==1.2.*"));
        assert!(meets("1.3.0", "!=1.2.*"));
    }

    #[test]
    fn a_compatible_release_misses_in_either_direction() {
        // `~=0.1.0` is `>=0.1.0, ==0.1.*`: below 0.1.0 is too old, above 0.1.x
        // is too new.
        let c = parse_specifier("~=0.1.0").unwrap();
        assert_eq!(violation(&c, &v("0.0.9")), Some(Kind::DistroTooOld));
        assert_eq!(violation(&c, &v("0.2.0")), Some(Kind::DistroTooNew));
        assert_eq!(violation(&c, &v("0.1.5")), None);
        // `~=1.4` is `>=1.4, ==1.*`.
        let c = parse_specifier("~=1.4").unwrap();
        assert_eq!(violation(&c, &v("1.3")), Some(Kind::DistroTooOld));
        assert_eq!(violation(&c, &v("2.0")), Some(Kind::DistroTooNew));
        assert_eq!(violation(&c, &v("1.9")), None);
    }

    #[test]
    fn compatible_release_and_exclusive_bounds() {
        assert!(meets("13.4.4", "~=13.4.4"));
        assert!(!meets("13.9", "~=13.4.4"));
        assert!(meets("2.5", "~=2.2"));
        assert!(!meets("3.0", "~=2.2"));
        // `<2` keeps out `2.0rc1`, which is a pre-release of 2; `>1` keeps out
        // `1.post1`, which is a post release of 1.
        assert!(!meets("2.0rc1", "<2"));
        assert!(meets("1.9", "<2"));
        assert!(!meets("1.0.post1", ">1.0"));
        assert!(meets("1.1", ">1.0"));
        assert!(meets("1.0", ">=1.0"));
    }

    #[test]
    fn a_specifier_that_is_not_understood_is_not_held_against_a_version() {
        assert!(parse_specifier("garbage").is_none());
        assert!(parse_specifier(">=1.0.*").is_none());
        assert!(parse_specifier("(>=1.0, <2)").is_some());
        assert!(parse_version("2.0.0rc1").is_some());
    }

    #[test]
    fn requirement_lines_split_into_name_bound_and_marker() {
        assert_eq!(
            parse_requirement("apache-tvm-ffi<=0.1.11,>=0.1.10"),
            Some((
                "apache-tvm-ffi".to_string(),
                "<=0.1.11,>=0.1.10".to_string(),
                None
            ))
        );
        assert_eq!(
            parse_requirement("uvicorn[standard] (>=0.15) ; sys_platform == \"linux\""),
            Some((
                "uvicorn".to_string(),
                "(>=0.15)".to_string(),
                Some("sys_platform == \"linux\"".to_string())
            ))
        );
        assert_eq!(
            parse_requirement("pkg @ https://example.org/pkg.whl"),
            Some(("pkg".to_string(), String::new(), None))
        );
        assert_eq!(parse_requirement("  "), None);
    }

    #[test]
    fn markers_follow_the_linux_target() {
        assert!(marker_applies("sys_platform == \"linux\""));
        assert!(!marker_applies("sys_platform == \"win32\""));
        assert!(!marker_applies(
            "platform_system == 'Windows' and python_version < '3.10'"
        ));
        assert!(marker_applies(
            "sys_platform == \"win32\" or platform_machine == \"x86_64\""
        ));
        assert!(marker_applies("platform_machine != \"aarch64\""));
        // The Python version is not evaluated, so the bound stays in force.
        assert!(marker_applies("python_version < \"3.10\""));
        // Unreadable (parenthesised) markers apply.
        assert!(marker_applies("(sys_platform == \"win32\")"));
        // Optional dependencies never apply.
        assert!(!marker_applies("extra == \"cuda\""));
    }

    fn node(name: &str, version: &str, requires: &[&str]) -> Node {
        Node {
            name: name.to_string(),
            version: version.to_string(),
            sha256: String::new(),
            requires_dist: requires.iter().map(|s| s.to_string()).collect(),
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        }
    }

    fn shipped<'a>(table: &'a [(usize, &'a str)]) -> impl FnMut(usize) -> Option<String> + 'a {
        move |i| {
            table
                .iter()
                .find(|(j, _)| *j == i)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_too_new_distro_dependency_is_reported_and_stays_distro() {
        let nodes = vec![
            node("tilelang", "0.1.12", &["apache-tvm-ffi>=0.1.10,<=0.1.11"]),
            node("apache-tvm-ffi", "0.1.11", &[]),
        ];
        let plan = plan(
            &nodes,
            &[false, true],
            &[true, false],
            shipped(&[(1, "0.1.14.post1")]),
        );
        assert_eq!(plan.rebuild, Vec::<usize>::new());
        assert_eq!(plan.conflicts.len(), 1);
        let c = &plan.conflicts[0];
        assert_eq!(c.kind, Kind::DistroTooNew);
        assert_eq!(c.dependent, "tilelang");
        assert!(c.message("openSUSE:Factory").contains("convention 59"));
    }

    #[test]
    fn a_too_old_distro_dependency_is_rebuilt_and_its_own_bounds_are_checked() {
        // open-webui pins redis==8.0.1; Factory has 7.0.1. Rebuilding redis
        // here makes redis a dependent: it needs `async-timeout>=5`, which the
        // distro ships at 4.0.
        let nodes = vec![
            node("open-webui", "0.12.0", &["redis==8.0.1"]),
            node("redis", "8.0.1", &["async-timeout>=5"]),
            node("async-timeout", "5.0.1", &[]),
        ];
        let plan = plan(
            &nodes,
            &[false, true, true],
            &[true, false, false],
            shipped(&[(1, "7.0.1"), (2, "4.0")]),
        );
        assert_eq!(plan.rebuild, vec![1, 2]);
        assert!(plan.conflicts.iter().all(|c| c.kind == Kind::DistroTooOld));
        assert!(
            plan.conflicts[0]
                .message("openSUSE:Factory")
                .contains("redis 8.0.1 is built in this project")
        );
    }

    #[test]
    fn a_dependent_the_distro_ships_is_not_checked() {
        // numba ships from the distro: its pin on llvmlite was met by the
        // distro's own build.
        let nodes = vec![
            node("numba", "0.65.0", &["llvmlite<0.48,>=0.47"]),
            node("llvmlite", "0.47.0", &[]),
        ];
        let plan = plan(
            &nodes,
            &[true, true],
            &[false, false],
            shipped(&[(1, "0.50.0")]),
        );
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn unknown_versions_optional_and_foreign_bounds_are_not_reported() {
        let nodes = vec![
            node(
                "app",
                "1.0",
                &[
                    "a>=2",
                    "b>=2 ; extra == 'test'",
                    "c>=2 ; sys_platform == 'win32'",
                    "d>=not-a-version",
                    "e",
                ],
            ),
            node("a", "2.0", &[]),
            node("b", "2.0", &[]),
            node("c", "2.0", &[]),
            node("d", "2.0", &[]),
            node("e", "2.0", &[]),
        ];
        // `a` has no known distro version; every other one is too old.
        let plan = plan(
            &nodes,
            &[false, true, true, true, true, true],
            &[true, false, false, false, false, false],
            shipped(&[(2, "1.0"), (3, "1.0"), (4, "1.0"), (5, "1.0")]),
        );
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    }

    #[test]
    fn only_bounded_distro_dependencies_of_built_nodes_are_looked_up() {
        let nodes = vec![
            node("app", "1.0", &["a>=1", "b"]),
            node("a", "1.0", &[]),
            node("b", "1.0", &[]),
            node("lib", "1.0", &["a>=1"]),
        ];
        assert_eq!(
            candidates(
                &nodes,
                &[false, true, true, false],
                &[true, false, false, false]
            ),
            vec![1]
        );
    }

    #[test]
    fn a_requested_root_older_in_the_distro_is_called_out() {
        let root = node("vllm", "0.31.0", &[]);
        let message = stale_root_message(&root, "0.30.0", "openSUSE:Factory").unwrap();
        assert!(message.contains("vllm 0.30.0") && message.contains("--force"));
        assert_eq!(
            stale_root_message(&root, "0.31.0", "openSUSE:Factory"),
            None
        );
        assert_eq!(
            stale_root_message(&root, "0.32.0", "openSUSE:Factory"),
            None
        );
        assert_eq!(stale_root_message(&root, "git", "openSUSE:Factory"), None);
    }

    #[test]
    fn fetch_parallel_keeps_the_answers_it_got() {
        let got = fetch_parallel(&[3, 5, 7], 2, |i| (i != 5).then(|| format!("v{i}")));
        assert_eq!(got.len(), 2);
        assert_eq!(got[&3], "v3");
        assert_eq!(got[&7], "v7");
        assert!(fetch_parallel(&[], 4, |_| Some(String::new())).is_empty());
    }
}

#[cfg(test)]
mod bound_violation_tests {
    use super::*;

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_newer_version_than_the_upper_bound_is_too_new() {
        let req = lines(&["apache-tvm-ffi<=0.1.11,>=0.1.10", "numpy>=1.23"]);
        let (kind, spec) = bound_violation(&req, "apache-tvm-ffi", "0.1.14.post1").unwrap();
        assert_eq!(kind, Kind::DistroTooNew);
        assert_eq!(spec, "<=0.1.11,>=0.1.10");
    }

    #[test]
    fn an_older_version_than_the_lower_bound_is_too_old() {
        let req = lines(&["opentelemetry-instrumentation==0.66b1"]);
        let (kind, _) = bound_violation(&req, "opentelemetry_instrumentation", "0.66b0").unwrap();
        assert_eq!(kind, Kind::DistroTooOld);
    }

    #[test]
    fn a_met_bound_an_extra_and_no_bound_are_none() {
        let req = lines(&["a>=1", "b>=5; extra == \"test\"", "c"]);
        assert!(bound_violation(&req, "a", "2.0").is_none());
        assert!(bound_violation(&req, "b", "1.0").is_none());
        assert!(bound_violation(&req, "c", "1.0").is_none());
    }
}

#[cfg(test)]
mod redist_tests {
    use super::*;
    use crate::redist::TestMode;

    fn node(name: &str, version: &str, requires: &[&str]) -> Node {
        Node {
            name: name.to_string(),
            version: version.to_string(),
            sha256: String::new(),
            requires_dist: requires.iter().map(|s| s.to_string()).collect(),
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        }
    }

    /// `nvidia-cublas>=13.1` is a bound on a wheel; `cuda-cublas` 13.8.0.4 is
    /// NVIDIA's scheme, so the bound is never held against it (and a bound the
    /// other way, `<13`, would not rebuild the component either).
    #[test]
    fn a_bound_on_a_cuda_wheel_is_not_compared_with_the_components_version() {
        let req = vec!["nvidia-cublas>=13.1".to_string(), "numpy>=2".to_string()];
        let nodes = vec![
            node("torch", "2.12", &["nvidia-cublas>=13.1", "numpy>=2"]),
            node("nvidia-cublas", "13.1.1.3", &[]),
            node("numpy", "2.3", &[]),
        ];
        let table = |i: usize| match i {
            1 => Some("9.0".to_string()),
            _ => Some("1.0".to_string()),
        };
        {
            let _off = TestMode::set(false);
            assert!(bound_violation(&req, "nvidia-cublas", "9.0").is_some());
            let p = plan(&nodes, &[false, true, true], &[true, false, false], table);
            assert!(p.conflicts.iter().any(|c| c.dependency == "nvidia-cublas"));
        }
        let _on = TestMode::set(true);
        assert!(bound_violation(&req, "nvidia-cublas", "9.0").is_none());
        assert!(candidates(&nodes, &[false, true, true], &[true, false, false]) == vec![2]);
        let p = plan(&nodes, &[false, true, true], &[true, false, false], table);
        assert!(p.conflicts.iter().all(|c| c.dependency != "nvidia-cublas"));
        assert!(p.rebuild.iter().all(|&i| i != 1));
        // Other bounds are still checked.
        assert!(bound_violation(&req, "numpy", "1.0").is_some());
    }
}
