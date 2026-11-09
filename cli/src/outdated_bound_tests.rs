use super::*;

/// python-celery: `python313-kombu >= 5.7.0` against the repository's 5.6.2
/// is outdated, so the dependency is kept and its update queued.
#[test]
fn a_bound_the_repository_copy_misses_keeps_the_dependency() {
    assert!(bound_still_unmet(Some("5.7.0"), Some("5.6.2")));
}

/// The same bound against a copy that meets it: not outdated, so the usual
/// availability check drops it as resolvable.
#[test]
fn a_bound_the_repository_copy_meets_drops_the_dependency() {
    assert!(!bound_still_unmet(Some("5.7.0"), Some("5.7.1")));
    assert!(!bound_still_unmet(Some("5.7.0"), Some("5.7.0")));
}

/// Without a bound the decision is what it was before bounds were read.
#[test]
fn without_a_bound_the_update_path_applies_as_before() {
    assert!(bound_still_unmet(None, Some("5.7.1")));
    assert!(bound_still_unmet(None, None));
}

/// A copy the repository's spec does not give a version for cannot be
/// compared: the update path decides, as before.
#[test]
fn an_unknown_repository_copy_keeps_the_update_path() {
    assert!(bound_still_unmet(Some("5.7.0"), None));
}

/// Versions compare as versions, not as text: 5.10.0 meets `>= 5.9.0`.
#[test]
fn versions_compare_numerically() {
    assert!(!bound_still_unmet(Some("5.9.0"), Some("5.10.0")));
    assert!(bound_still_unmet(Some("5.10.0"), Some("5.9.0")));
}
