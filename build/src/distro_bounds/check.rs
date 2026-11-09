use super::*;

/// How a version that misses a bound misses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Above an upper bound or an exact pin.
    DistroTooNew,
    /// Below a lower bound or an exact pin.
    DistroTooOld,
}

pub(super) fn parse_version(text: &str) -> Option<Version> {
    crate::version::parse_pep440(text)
}

/// `>=1.2,<2` (or `(>=1.2, <2)`) as a bound. `None` when it is not understood,
/// so a bound this cannot read is never held against a version.
pub(crate) fn parse_specifier(text: &str) -> Option<VersionSpecifiers> {
    let text = text.trim().trim_start_matches('(').trim_end_matches(')');
    VersionSpecifiers::from_str(text).ok()
}

/// Which way `version` misses `bound`, or `None` when it meets it.
pub(super) fn violation(bound: &VersionSpecifiers, version: &Version) -> Option<Kind> {
    let failed = bound.iter().find(|s| !s.contains(version))?;
    // `~=X` is two-sided (`>=X` and `==X.*`): a version below `X` is too old,
    // one at or above the next prefix bump is too new.
    Some(match failed.operator() {
        Operator::GreaterThan | Operator::GreaterThanEqual => Kind::DistroTooOld,
        Operator::TildeEqual | Operator::Equal | Operator::EqualStar | Operator::ExactEqual
            if version < failed.version() =>
        {
            Kind::DistroTooOld
        }
        _ => Kind::DistroTooNew,
    })
}

/// One `Requires-Dist` line: the name, the version bound (empty when there is
/// none) and the environment marker.
pub(crate) fn parse_requirement(line: &str) -> Option<(String, String, Option<String>)> {
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
pub(crate) fn marker_applies(marker: &str) -> bool {
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

pub(super) fn clause_applies(clause: &str) -> bool {
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
    let wanted = crate::osc::project_key(dependency);
    for line in requires_dist {
        let Some((name, spec, marker)) = parse_requirement(line) else {
            continue;
        };
        if crate::osc::project_key(&name) != wanted
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
                "{head}: relax the bound in {}'s spec (convention 75 (section 59)); only a real API break \
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
