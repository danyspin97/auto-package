use super::changes_normalise::{is_older, normalise_with_notes};
use super::changes_normalise_tests::{HEADER, entry};
use crate::version::Scheme;

#[test]
fn pep440_orders_release_candidates_before_the_release_and_posts_after() {
    assert!(is_older("1.0rc1", "1.0", Scheme::Pep440));
    assert!(is_older("1.0", "1.0.post1", Scheme::Pep440));
    assert!(is_older("1.0rc1", "1.0.post1", Scheme::Pep440));
    assert!(!is_older("1.0", "1.0rc1", Scheme::Pep440));
    assert!(!is_older("1.0.post1", "1.0", Scheme::Pep440));
}

#[test]
fn pep440_orders_a_dev_release_before_the_alpha_of_the_same_number() {
    assert!(is_older("1.0.dev1", "1.0a1", Scheme::Pep440));
    assert!(!is_older("1.0a1", "1.0.dev1", Scheme::Pep440));
}

#[test]
fn a_release_that_cannot_be_ordered_is_not_older() {
    assert!(!is_older("%{version}", "1.0", Scheme::Pep440));
    assert!(!is_older("1.0", "%{version}", Scheme::Pep440));
}

#[test]
fn the_generic_order_is_unchanged_for_generic_versions() {
    let pairs = [
        ("5.3", "5.4", true),
        ("5.4", "5.3", false),
        ("2.9", "2.10", true),
        ("2.10", "2.9", false),
        ("1.0", "1.0", false),
        ("3.0~rc1", "3.0", true),
        ("1.14.1", "1.14.2", true),
    ];
    for (release, version, older) in pairs {
        assert_eq!(
            is_older(release, version, Scheme::Generic),
            older,
            "{release} against {version}"
        );
    }
}

#[test]
fn a_dev_release_entry_is_kept_under_pep440_and_dropped_under_the_generic_order() {
    // In PEP 440 the 1.0.dev1 entry is older than the spec's 1.0a1, so it is
    // kept below the 1.0a1 entry. The generic order compares `dev` after `a`
    // and calls it newer, so the entry goes.
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 1.0a1",
    );
    let older = entry(HEADER, "- Update to 1.0.dev1\n- Refresh o.patch");
    let branch = format!("{newer}{older}");
    let (pep, _) = normalise_with_notes("", &branch, "1.0a1", "pkg", Scheme::Pep440);
    let pep = pep.unwrap_or(branch.clone());
    assert!(pep.contains("Update to 1.0.dev1"), "{pep}");
    let (generic, _) = normalise_with_notes("", &branch, "1.0a1", "pkg", Scheme::Generic);
    let generic = generic.unwrap_or(branch.clone());
    assert!(!generic.contains("Update to 1.0.dev1"), "{generic}");
}
