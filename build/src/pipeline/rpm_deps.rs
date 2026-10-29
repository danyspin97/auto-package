//! Whether an RPM satisfies a dependency and what version a spec names.

use super::*;

/// Returns true if any RPM file in `rpms_dir` provides `dep`.
///
/// `dep` is a raw RPM dependency string like `python313-Cython>=3.1`. The
/// name and the (optional) version operator + required version are parsed
/// via [`parse_rpm_dep_constraint`], then each matching RPM filename is
/// unpacked by [`extract_rpm_filename_version`] and compared using
/// [`rpm_version_satisfies`].
///
/// The pre-review fix only compared by name-prefix and ignored the version
/// operator entirely — so e.g. a fake `python-foo-0.1-1.1.x86_64.rpm`
/// satisfied `python-foo>=99`, and the dep got silently filtered out of the
/// "ask the agent to look at this" list even though no actually-usable RPM
/// existed. OBS would then keep reporting the same dep as unresolvable until
/// the attempt counter ran out.
/// Whether `dep` (an RPM name, flavor prefix included) is one of the packages
/// OBS reported as older than a bound, given as `fuzzy_key`s of their bare names.
pub(super) fn is_outdated_dep(outdated_keys: &[String], dep: &str) -> bool {
    let key = osc::fuzzy_key(osc::strip_rpm_prefix(dep));
    outdated_keys.contains(&key)
}

pub(super) fn rpm_satisfies_dep(rpms_dir: &Path, dep: &str) -> bool {
    let (pkg_name, constraint) = parse_rpm_dep_constraint(dep);
    let prefix = format!("{pkg_name}-");
    std::fs::read_dir(rpms_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(prefix.as_str()) {
                return false;
            }
            let Some(ver) = extract_rpm_filename_version(pkg_name, &name) else {
                return false;
            };
            match &constraint {
                Some((op, required)) => rpm_version_satisfies(ver, op, required),
                None => true,
            }
        })
}

/// Split an RPM dependency string `python313-Cython>=3.1` into
/// `("python313-Cython", Some((">=", "3.1")))`. Operators considered, in
/// precedence order to avoid matching `>=` as `>` first: `>=`, `<=`, `==`,
/// `!=`, `>`, `<`, `=`. A bare `name` (no operator) returns `(name, None)`
/// — the caller treats "any version will do" as satisfied by presence of
/// the name alone.
pub(super) fn parse_rpm_dep_constraint(dep: &str) -> (&str, Option<(&str, &str)>) {
    const OPS: &[&str] = &[">=", "<=", "==", "!=", ">", "<", "="];
    for op in OPS {
        if let Some(i) = dep.find(op) {
            let (name, rest) = dep.split_at(i);
            let version = rest[op.len()..].trim();
            return (name.trim(), Some((*op, version)));
        }
    }
    (dep.trim(), None)
}

/// Pull `<version>` out of an RPM filename of the form
/// `<name>-<version>-<release>.<arch>.rpm`.
///
/// `name` is passed in so no name/version ambiguity exists (name may itself
/// contain `-`). Returns `None` for a filename whose shape doesn't match
/// (not stripped of `.rpm`, missing a `-<release>` or `.<arch>` segment).
pub(super) fn extract_rpm_filename_version<'a>(name: &str, filename: &'a str) -> Option<&'a str> {
    let stem = filename.strip_suffix(".rpm")?;
    // stem = <name>-<version>-<release>.<arch>
    let after_name = stem.strip_prefix(name)?.strip_prefix('-')?;
    // after_name = <version>-<release>.<arch>
    let (ver_rel, arch) = after_name.rsplit_once('.')?;
    const ARCHES: &[&str] = &[
        "noarch", "x86_64", "i586", "i686", "aarch64", "ppc64le", "s390x", "armv7hl", "riscv64",
    ];
    if !ARCHES.contains(&arch) {
        return None;
    }
    // Neither version nor release holds a '-', so exactly one separates them.
    // Anything more means `name` is only a prefix of a longer package name
    // (`python313-foo-bar-1.0-1.1`, `python313-foo-devel-1.0-1.1`).
    let (version, release) = ver_rel.split_once('-')?;
    if release.contains('-') || !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(version)
}

/// Evaluate `candidate <op> required` using numeric-aware segment compare.
///
/// Both strings are split on `.`/`-`; each segment compares numerically when
/// both sides parse as integers (so `20 > 2`, not `"20" < "2"`
/// lexicographically) and lexicographically otherwise. Missing trailing
/// segments default to "0" so `1.2 == 1.2.0` and `1.2 < 1.2.3`.
///
/// Not a full RPM EVR implementation — doesn't understand `~`, `^`, or an
/// explicit epoch. It is correct for every Python-RPM case the project has
/// in practice, and strictly tighter than the pre-review substring-only
/// behavior; a true full-fidelity EVR comparator belongs in `osc`/`rpm`
/// crate wrappers, not here.
pub(super) fn rpm_version_satisfies(candidate: &str, op: &str, required: &str) -> bool {
    use std::cmp::Ordering;
    let a: Vec<&str> = candidate.split(['.', '-']).collect();
    let b: Vec<&str> = required.split(['.', '-']).collect();
    let n = a.len().max(b.len());
    let mut ord = Ordering::Equal;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or("0");
        let y = b.get(i).copied().unwrap_or("0");
        let segment_ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => xn.cmp(&yn),
            _ => x.cmp(y),
        };
        if segment_ord != Ordering::Equal {
            ord = segment_ord;
            break;
        }
    }
    match op {
        ">=" => !matches!(ord, Ordering::Less),
        "<=" => !matches!(ord, Ordering::Greater),
        ">" => matches!(ord, Ordering::Greater),
        "<" => matches!(ord, Ordering::Less),
        "==" | "=" => matches!(ord, Ordering::Equal),
        "!=" => !matches!(ord, Ordering::Equal),
        _ => true,
    }
}

/// True iff the spec on disk declares a `Version:` other than `version`.
///
/// Compares the parsed `Version:` tag value for exact string equality, not
/// a substring check. The previous `contains("Version: 1.2")` matched inside
/// `Version: 1.20`, so a request for `1.2` wrongly reported "spec matches"
/// against a spec actually declaring `1.20`, and the pipeline would short-
/// circuit as `AlreadyBuilt` for the wrong version.
///
/// `true` on an unreadable spec or one with no `Version:` at all so the
/// caller re-generates rather than skipping work it has no evidence it
/// already did.
pub(super) fn spec_version_differs(spec_path: &Path, version: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(spec_path) else {
        return true;
    };
    let Some(header) = crate::spec::parse_header(&text) else {
        return true;
    };
    header.version != version
}

/// A version as a release directory names it: digits and dots, then any
/// pre-release, post, dev, or local suffix (`1.2rc1`, `2.4.0a0`, `1.2+git2024`).
pub(super) static RELEASE_DIR_VERSION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(
        r"^[0-9]+(\.[0-9]+)*([a-z]+[0-9]*|rc[0-9]+|\+[A-Za-z0-9_.]+|\.post[0-9]+|\.dev[0-9]+)*$",
    )
    .expect("a valid version pattern")
    });
