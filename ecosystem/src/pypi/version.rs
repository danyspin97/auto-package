/// Three-way result of a PEP 440 comparison between two version strings.
///
/// Collapsing `Unparseable` into `false`/`NotNewer` (as `is_newer_pep440`
/// used to) made the update sweep classify a package it *couldn't actually
/// compare* as `UpToDate` — the Oct 6 review's probe with a non-PEP 440
/// packaged version and a real-newer upstream reproduced the false-current
/// label. Callers that want "yes/no newer" still have `is_newer_pep440`;
/// callers that need to distinguish "I checked, it's current" from "I
/// couldn't check" (every update-plan classifier) use this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCompare {
    Newer,
    NotNewer,
    Unparseable,
}

/// Compare two PEP 440 version strings three-way, strictly: a version that is
/// not PEP 440 is `Unparseable` (no generic fallback, no epoch or `v`/`r`
/// prefix handling; [`crate::version::cmp`] has those).
pub fn compare_pep440(latest: &str, packaged: &str) -> VersionCompare {
    let (Some(l), Some(p)) = (
        crate::version::parse_pep440(latest),
        crate::version::parse_pep440(packaged),
    ) else {
        return VersionCompare::Unparseable;
    };
    if l > p {
        VersionCompare::Newer
    } else {
        VersionCompare::NotNewer
    }
}

/// True iff `latest` is a strictly newer version than `packaged` in PyPI's
/// order ([`crate::version::cmp`] with [`crate::version::Scheme::Pep440`]).
///
/// Returns `false` for an unparseable input too; callers that must
/// distinguish "cannot compare" from "not newer" need
/// [`crate::version::cmp`]'s `Option` instead — see [`VersionCompare`] for why.
pub fn is_newer_pep440(latest: &str, packaged: &str) -> bool {
    crate::version::cmp(latest, packaged, crate::version::Scheme::Pep440)
        == Some(std::cmp::Ordering::Greater)
}
