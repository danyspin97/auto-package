use super::*;
use crate::obs_status::{self, ResultEntry};

const COUCHDB: &str = "couchdb";

/// A `_result` document with one `<status>` per (repository, package, code)
/// and the given details text on an unresolvable one.
fn results(statuses: &[(&str, &str, &str, Option<&str>)]) -> Vec<ResultEntry> {
    let mut xml = String::from("<resultlist state=\"abc\">\n");
    for (repo, pkg, code, details) in statuses {
        xml.push_str(&format!(
            "  <result project=\"X\" repository=\"{repo}\" arch=\"x86_64\" code=\"published\" state=\"published\">\n    <status package=\"{pkg}\" code=\"{code}\">"
        ));
        match details {
            Some(d) => xml.push_str(&format!("<details>{d}</details></status>\n")),
            None => xml.push_str("</status>\n"),
        }
        xml.push_str("  </result>\n");
    }
    xml.push_str("</resultlist>\n");
    obs_status::try_parse_results(&xml).unwrap()
}

const NOTHING: &str = "nothing provides js-devel needed by couchdb";

#[test]
fn unresolvable_in_every_repository_is_skipped_with_the_missing_package() {
    let entries = results(&[
        (
            "openSUSE_Tumbleweed",
            COUCHDB,
            "unresolvable",
            Some(NOTHING),
        ),
        ("openSUSE_Leap_16.0", COUCHDB, "unresolvable", Some(NOTHING)),
    ]);
    assert_eq!(
        origin_unresolvable_reason(&entries, COUCHDB).as_deref(),
        Some("nothing provides js-devel")
    );
}

#[test]
fn one_repository_that_succeeded_is_branched() {
    let entries = results(&[
        (
            "openSUSE_Tumbleweed",
            COUCHDB,
            "unresolvable",
            Some(NOTHING),
        ),
        ("openSUSE_Leap_16.0", COUCHDB, "succeeded", None),
    ]);
    assert_eq!(origin_unresolvable_reason(&entries, COUCHDB), None);
}

#[test]
fn a_failed_build_is_branched_because_an_update_may_fix_it() {
    let entries = results(&[
        ("openSUSE_Tumbleweed", COUCHDB, "failed", None),
        ("openSUSE_Leap_16.0", COUCHDB, "unresolvable", Some(NOTHING)),
    ]);
    assert_eq!(origin_unresolvable_reason(&entries, COUCHDB), None);
}

#[test]
fn a_dirty_unresolvable_result_is_not_decisive() {
    let xml = "<resultlist state=\"abc\"><result project=\"X\" repository=\"r\" arch=\"x86_64\" code=\"published\" state=\"published\" dirty=\"true\"><status package=\"couchdb\" code=\"unresolvable\"/></result></resultlist>";
    let entries = obs_status::try_parse_results(xml).unwrap();
    assert_eq!(origin_unresolvable_reason(&entries, COUCHDB), None);
}

#[test]
fn an_origin_with_no_results_is_branched_as_a_new_package() {
    let entries = results(&[]);
    assert_eq!(origin_unresolvable_reason(&entries, COUCHDB), None);
    // Results for another package in the same document do not count either.
    let entries = results(&[(
        "openSUSE_Tumbleweed",
        "other",
        "unresolvable",
        Some(NOTHING),
    )]);
    assert_eq!(origin_unresolvable_reason(&entries, COUCHDB), None);
}

#[test]
fn unresolvable_without_a_named_package_still_skips() {
    let entries = results(&[("openSUSE_Tumbleweed", COUCHDB, "unresolvable", None)]);
    assert_eq!(
        origin_unresolvable_reason(&entries, COUCHDB).as_deref(),
        Some("OBS reports it unresolvable in every repository")
    );
}
