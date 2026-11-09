//! Whether a package's `BuildRequires` need a newer version of a dependency
//! than its origin project has, so the dependency has to be branched and
//! built before the package, or whether the package can be tested against the
//! origin's binaries as they are.
//!
//! The decision is pure: the caller gives the package's requirements (from
//! [`crate::spec::build_requirements`]), the names the dependency is required
//! by, the version the dependency's origin has, what its providers give for
//! the capabilities it is required by, and the version it would be updated
//! to. Versions are ordered with [`crate::version::cmp`], never as strings.

use super::capability::{accepts, strip_release};
use super::requirements::requirement_text;
use super::*;
use crate::spec::{Bound, Requirement};

/// What a package's requirements need of a dependency that has a newer
/// release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepNeed {
    /// The origin's version meets every bound on the dependency, or none is
    /// stated: the package builds against the origin's binaries.
    Origin,
    /// A bound the origin's version misses and the new version meets: the
    /// dependency is updated first. The requirement as the spec writes it.
    Update(String),
    /// A bound neither the origin's version nor the new one meets: updating
    /// the dependency would not help, and the build says what is missing.
    Unmet(String),
    /// A bound that cannot be compared with the versions (a macro, a version
    /// that is not one), and none the origin misses for sure.
    Unknown(String),
}

/// What `reqs` need of a dependency required by any of `names` (a package
/// name, or a capability as the spec writes it with its bound), whose origin
/// has the source package at `origin_version` and whose update would bring
/// `new_version`.
///
/// The origin side of a capability bound is the version its provider's
/// `Provides:` gives, from `provided` (`(name, version)` as in `names`); a
/// name without one there is compared with `origin_version`, as a package
/// name is (its RPMs carry the package's version). The update side is always
/// `new_version`: the provide of the new release is not known until it is
/// built, and the package's new version is what the update brings.
///
/// A bound the update meets and the origin misses decides first (`Update`),
/// then one that cannot be compared (`Unknown`), then one neither meets
/// (`Unmet`). A bound with a release (`= 1.2-3`) is compared with it only
/// against a version that has one (see [`meets`]).
pub fn dependency_need(
    reqs: &[Requirement],
    names: &[String],
    origin_version: &str,
    provided: &[(String, String)],
    new_version: &str,
) -> DepNeed {
    let mut unknown = None;
    let mut unmet = None;
    for req in reqs {
        let (Some(bound), Some(text)) = (&req.bound, required_by(req, names)) else {
            continue;
        };
        let origin = provided
            .iter()
            .find(|(name, _)| *name == text)
            .map_or(origin_version, |(_, version)| version.as_str());
        match (meets(origin, bound), meets(new_version, bound)) {
            (Some(false), Some(true)) => return DepNeed::Update(text),
            (None, _) | (Some(false), None) => {
                unknown.get_or_insert(text);
            }
            (Some(false), Some(false)) => {
                unmet.get_or_insert(text);
            }
            (Some(true), _) => {}
        }
    }
    match (unknown, unmet) {
        (Some(text), _) => DepNeed::Unknown(text),
        (None, Some(text)) => DepNeed::Unmet(text),
        (None, None) => DepNeed::Origin,
    }
}

/// The requirement's text when it names the dependency by one of `names`.
fn required_by(req: &Requirement, names: &[String]) -> Option<String> {
    let (name, text) = match &req.dep {
        BuildRequire::Package(n) => (n.as_str(), requirement_text(req, n)),
        BuildRequire::Capability(c) => (c.as_str(), requirement_text(req, c)),
    };
    names
        .iter()
        .any(|n| n == name || *n == text)
        .then_some(text)
}

/// The text of the first bounded requirement in `reqs` that names one of
/// `names` (matched as [`dependency_need`] matches them), `None` when no
/// requirement on them has a bound.
pub fn bounded_requirement(reqs: &[Requirement], names: &[String]) -> Option<String> {
    reqs.iter()
        .filter(|req| req.bound.is_some())
        .find_map(|req| required_by(req, names))
}

/// Whether `version` meets `bound`, `None` when the two cannot be compared.
///
/// Compared as RPM compares a dependency with a candidate: the epochs first,
/// a missing epoch counting as 0 on either side, then the versions. The
/// release is compared only when the bound has one and the version has one
/// too; otherwise both sides are compared without it, so a provide's `1.2-3`
/// is `1.2` against a bound `>= 1.2`, and `= 1.2-4` is met by `1.2`.
fn meets(version: &str, bound: &Bound) -> Option<bool> {
    let has_release = |v: &str| strip_release(v) != v;
    let (have, wanted) = if has_release(&bound.version) && has_release(version) {
        (version, bound.version.as_str())
    } else {
        (strip_release(version), strip_release(&bound.version))
    };
    let ord = crate::version::cmp(have, wanted, crate::version::Scheme::Generic)?;
    Some(accepts(&bound.op, ord))
}
