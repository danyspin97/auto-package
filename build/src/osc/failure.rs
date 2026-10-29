//! Build status and failure classification: the status type, the failure kinds and
//! the unresolvable names and version misses read from a build log.

use super::*;

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
pub(super) fn classify_build_failure(log: &str) -> FailureKind {
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

/// The `nothing provides` requirements named on one line of a build log or of
/// OBS's `<details>` text, each without the marker and without any
/// `needed by ...` clause.
///
/// OBS's `<details>` for an `unresolvable` package joins every missing
/// requirement onto ONE line (`nothing provides python314-fastar, nothing
/// provides python313-fastar, ...`), while a build log has one per line, so a
/// line may hold several. Every segment is read, not only the first.
pub(super) fn nothing_provides_segments(line: &str) -> Vec<String> {
    const MARKER: &str = "nothing provides ";
    let line = line.trim();
    let line = line
        .strip_prefix("unresolvable:")
        .unwrap_or(line)
        .trim_start();
    // Only a line that starts with the phrase names requirements: one that
    // merely mentions it somewhere in prose must not count.
    let Some(mut rest) = line.strip_prefix(MARKER) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    loop {
        let (segment, next) = match rest.find(MARKER) {
            Some(i) => (&rest[..i], Some(&rest[i + MARKER.len()..])),
            None => (rest, None),
        };
        let segment = segment.trim().trim_end_matches(',').trim_end();
        let segment = segment
            .split(" needed by ")
            .next()
            .unwrap_or(segment)
            .trim();
        if !segment.is_empty() {
            segments.push(segment.to_string());
        }
        match next {
            Some(n) => rest = n,
            None => break,
        }
    }
    segments
}

/// The package names in one `nothing provides` segment. Normally just the
/// first word (version constraint dropped); a rich dependency such as
/// `(python313-tomli >= 2.0.0 if python313-base < 3.11)` also carries the
/// boolean keywords `if`, `unless`, `and`, ... which are not packages.
pub(super) fn segment_package_names(segment: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &["if", "unless", "else", "and", "or", "with", "without"];
    let is_keyword = |w: &str| KEYWORDS.contains(&w);
    let words: Vec<&str> = segment.split_whitespace().collect();
    let rich = words.iter().any(|w| is_keyword(w.trim_matches(['(', ')'])));
    if !rich {
        let first = words.first().copied().unwrap_or(segment);
        let pkg = clean_unresolvable_name(first);
        return if pkg.is_empty() {
            Vec::new()
        } else {
            vec![pkg.to_string()]
        };
    }
    let mut names = Vec::new();
    // The word after a bare comparison operator (`>=`, `<`, ...) is its
    // version, not a package: `1.2` in `(foo >= 1.2 and bar)`.
    let mut after_operator = false;
    for w in words {
        let w = w.trim_matches(['(', ')']);
        // A comparison may stand alone (`>=`) or be glued to a name (`foo>=1.2`).
        let (name, comparison) = match w.find(['<', '>', '=', '!']) {
            Some(at) => (&w[..at], Some(&w[at..])),
            None => (w, None),
        };
        if let Some(cmp) = comparison {
            // A bare operator leaves its version to the next word; a glued
            // `>=1.2` carries it, and the next word is a package again.
            after_operator = cmp.trim_start_matches(['<', '>', '=', '!']).is_empty();
        } else if std::mem::take(&mut after_operator) || is_keyword(w) {
            continue;
        }
        // A digit may start a package name (`3proxy`, `python314-foo`): only
        // what is not a name at all, such as a stray `)` or `(`, is dropped.
        let name = clean_unresolvable_name(name);
        let looks_like_name = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
            // `1.2.3` is a version, which no package is called.
            && !name.chars().all(|c| c.is_ascii_digit() || c == '.');
        if looks_like_name {
            names.push(name.to_string());
        }
    }
    names
}

/// Extract RPM package names from "nothing provides <pkg>" lines in a build log.
///
/// Handles both bare `nothing provides <pkg>` and `unresolvable: nothing provides <pkg>`,
/// several requirements joined onto one line (OBS's `<details>` form), strips
/// any trailing `needed by ...` clause and any version constraint, and
/// deduplicates.
pub(super) fn extract_unresolvable(log: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for line in log.lines() {
        for segment in nothing_provides_segments(line) {
            for pkg in segment_package_names(&segment) {
                if seen.insert(pkg.clone()) {
                    result.push(pkg);
                }
            }
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
pub(super) fn clean_unresolvable_name(pkg: &str) -> &str {
    // Only an unbalanced parenthesis is decoration: the `()(64bit)` of a
    // soname capability (`libcuda.so.1()(64bit)`) is part of its name.
    let balance = |s: &str| s.matches('(').count() as isize - s.matches(')').count() as isize;
    let mut pkg = pkg;
    while pkg.starts_with('(') && balance(pkg) > 0 {
        pkg = &pkg[1..];
    }
    while pkg.ends_with(')') && balance(pkg) < 0 {
        pkg = &pkg[..pkg.len() - 1];
    }
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
pub(super) fn extract_unresolvable_detail(log: &str) -> Vec<String> {
    let mut result = Vec::new();
    let lines: Vec<&str> = log.lines().collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        let bare = line
            .strip_prefix("unresolvable:")
            .unwrap_or(line)
            .trim_start();
        // The solver's other complaints name no package to queue, but they
        // are what separates "needs building" from "the spec asks for the
        // impossible", so they travel with the detail.
        if bare.starts_with("have choice for ") || bare.starts_with("conflict for providers of ") {
            result.push(line.to_string());
            continue;
        }
        let segments = nothing_provides_segments(line);
        match segments.as_slice() {
            [] => continue,
            [_] => result.push(line.to_string()),
            many => result.extend(many.iter().map(|s| format!("nothing provides {s}"))),
        }
        if let Some(next) = lines.get(i + 1) {
            let next = next.trim();
            if next.starts_with('(') {
                result.push(next.to_string());
            }
        }
    }
    result
}

/// One `have choice for <capability> needed by <package>: <provider> ...` line
/// of the solver: it names no missing package, only several that could provide
/// what a package needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    pub capability: String,
    pub needed_by: String,
    pub providers: Vec<String>,
}

/// The provider choices in `detail`.
pub fn provider_choices(detail: &[String]) -> Vec<ProviderChoice> {
    detail
        .iter()
        .filter_map(|line| {
            let line = line.trim();
            let line = line
                .strip_prefix("unresolvable:")
                .unwrap_or(line)
                .trim_start();
            let rest = line.strip_prefix("have choice for ")?;
            let (capability, rest) = rest.split_once(" needed by ")?;
            let (needed_by, providers) = rest.split_once(':')?;
            Some(ProviderChoice {
                capability: capability.trim().to_string(),
                needed_by: needed_by.trim().to_string(),
                providers: providers.split_whitespace().map(str::to_string).collect(),
            })
        })
        .collect()
}

/// A package OBS has at a version that misses a constraint a spec wrote
/// (`nothing provides <name> >= V` followed by `(got version W)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMiss {
    /// Bare package name, flavors collapsed.
    pub name: String,
    /// The version OBS has, without its release.
    pub got: String,
    /// `true` when it is newer than an upper bound or a pin allows; `false` when
    /// older than a lower bound or a pin.
    pub too_new: bool,
    /// The version the constraint names when it is a lower bound or a pin
    /// (`>= 0.66b1`, `= 0.66b1`): the release to bring the package to, so a
    /// family of packages pinned together stays in step.
    pub required: Option<String>,
}

/// A rich dependency (`nothing provides (python314-trio >= 0.25.1 with
/// python314-trio < 0.34)`) names no version OBS has, so [`version_misses`]
/// cannot see that the bound is what fails. Each such line whose package
/// `shipped` knows a version of (the bare name in, the version out) and whose
/// clauses that version misses is replaced by the plain form of the first clause
/// it misses, with the `(got version ...)` line after it; every other line is
/// kept as it was.
pub fn annotate_rich_misses(
    detail: Vec<String>,
    shipped: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::with_capacity(detail.len());
    for line in detail {
        match rich_miss(&line, shipped) {
            Some((plain, got)) => {
                out.push(plain);
                out.push(got);
            }
            None => out.push(line),
        }
    }
    out
}

fn rich_miss(line: &str, shipped: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let line = line.trim();
    let line = line
        .strip_prefix("unresolvable:")
        .unwrap_or(line)
        .trim_start();
    let rest = line.strip_prefix("nothing provides ")?.trim_start();
    let group = rest.split(" needed by ").next()?.trim();
    let inner = group.strip_prefix('(')?.strip_suffix(')')?;
    if inner.contains(" or ") {
        return None;
    }
    let clauses: Vec<&str> = inner.split(" with ").map(str::trim).collect();
    let (name, _) = clauses.first()?.split_once(char::is_whitespace)?;
    let bare = missing_package_names(&[name.to_string()])
        .into_iter()
        .next()?;
    let got = shipped(&bare)?;
    let got_v: pep440_rs::Version = got.parse().ok()?;
    clauses.iter().find_map(|clause| {
        let (clause_name, constraint) = clause.split_once(char::is_whitespace)?;
        let Some(Some(_)) = crate::spec::rpm_constraint_miss(constraint, &got_v) else {
            return None;
        };
        Some((
            format!("nothing provides {clause_name} {}", constraint.trim()),
            format!("(got version {got}-0)"),
        ))
    })
}

/// The version misses in `detail`. A package here is not missing: too old, it
/// is brought up to date; too new, the bound in the spec is what is wrong.
pub fn version_misses(detail: &[String]) -> Vec<VersionMiss> {
    let mut out: Vec<VersionMiss> = Vec::new();
    // `osc results --verbose` puts the `(got version ...)` on the same line as
    // its `nothing provides` (`nothing provides x < 1, (got version 2-1.4)`),
    // a build log on the next one: read both as two lines.
    let detail: Vec<String> = detail
        .iter()
        .flat_map(|l| match l.find("(got version") {
            Some(at) if at > 0 => vec![
                l[..at].trim().trim_end_matches(',').trim().to_string(),
                l[at..].trim().to_string(),
            ],
            _ => vec![l.clone()],
        })
        .collect();
    for pair in detail.windows(2) {
        let (line, next) = (pair[0].trim(), pair[1].trim());
        let Some(got_text) = next.strip_prefix("(got version") else {
            continue;
        };
        let Some(rest) = line.strip_prefix("nothing provides ") else {
            continue;
        };
        let (name, constraint) = rest
            .split_once(char::is_whitespace)
            .map(|(n, c)| (n, c.trim()))
            .unwrap_or((rest, ""));
        let got = got_text
            .trim()
            .trim_end_matches(')')
            .split(['-', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let Ok(got_v) = got.parse::<pep440_rs::Version>() else {
            continue;
        };
        let Some(Some(too_new)) = crate::spec::rpm_constraint_miss(constraint, &got_v) else {
            continue;
        };
        let Some(bare) = missing_package_names(&[name.to_string()])
            .into_iter()
            .next()
        else {
            continue;
        };
        let required = ["<=", ">=", "==", "<", ">", "="]
            .iter()
            .find_map(|op| constraint.strip_prefix(op).map(|r| (*op, r.trim())))
            .filter(|(op, _)| matches!(*op, ">=" | "==" | "="))
            .map(|(_, r)| r.split(['-', ' ']).next().unwrap_or(r).to_string());
        if !out.iter().any(|m| fuzzy_key(&m.name) == fuzzy_key(&bare)) {
            out.push(VersionMiss {
                name: bare,
                got,
                too_new,
                required,
            });
        }
    }
    out
}

/// The packages in `detail` OBS has at a version older than a constraint asks
/// for.
pub fn outdated_packages(detail: &[String]) -> Vec<(String, Option<String>)> {
    version_misses(detail)
        .into_iter()
        .filter(|m| !m.too_new)
        .map(|m| (m.name, m.required))
        .collect()
}
