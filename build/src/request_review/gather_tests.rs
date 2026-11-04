use std::collections::HashMap;

use super::fetch::ReadFailure;
use super::fetch_fake::FakeRequestObs;
use super::gather::{ReviewError, gather};
use super::ids;
use crate::osc::parse_listing;

const REQUEST: &str = include_str!("../testdata/request_review/request_1394662.xml");
const SOURCE_LISTING: &str = include_str!("../testdata/request_review/listing_source.xml");
const TARGET_LISTING: &str = include_str!("../testdata/request_review/listing_target.xml");
const OLD_SPEC: &str = include_str!("../testdata/request_review/cracklib_old.spec");
const NEW_SPEC: &str = include_str!("../testdata/request_review/cracklib_new.spec");
const OLD_CHANGES: &str = include_str!("../testdata/request_review/cracklib_old.changes");
const NEW_CHANGES: &str = include_str!("../testdata/request_review/cracklib_new.changes");
const OLD_PATCH: &str = include_str!("../testdata/request_review/visibility_old.patch");
const NEW_PATCH: &str = include_str!("../testdata/request_review/visibility_new.patch");

const SP: &str = "home:dspinella:auto-package:branches:Base:System";
const TP: &str = "Base:System";

/// The fake OBS holding request 1394662.
pub(super) fn obs() -> FakeRequestObs {
    let mut o = FakeRequestObs::default();
    o.request.insert("1394662".into(), REQUEST.into());
    let key =
        |p: &str, rev: Option<&str>| (p.to_string(), "cracklib".to_string(), rev.map(String::from));
    o.listings
        .insert(key(SP, Some("3")), parse_listing(SOURCE_LISTING));
    o.listings
        .insert(key(TP, None), parse_listing(TARGET_LISTING));
    let mut text = |p: &str, rev: Option<&str>, name: &str, body: &str| {
        o.files.insert(
            (
                p.into(),
                "cracklib".into(),
                name.into(),
                rev.map(String::from),
            ),
            body.into(),
        );
    };
    text(SP, Some("3"), "cracklib.spec", NEW_SPEC);
    text(TP, None, "cracklib.spec", OLD_SPEC);
    text(SP, Some("3"), "cracklib.changes", NEW_CHANGES);
    text(TP, None, "cracklib.changes", OLD_CHANGES);
    text(
        SP,
        Some("3"),
        "0002-cracklib-2.9.2-visibility.patch",
        NEW_PATCH,
    );
    text(TP, None, "0002-cracklib-2.9.2-visibility.patch", OLD_PATCH);
    text(TP, None, "0001-cracklib-magic.diff", "magic\n");
    text(SP, Some("3"), "0001-cracklib-magic.diff", "magic\n");
    o.diff = Some("Index: cracklib.spec\n".into());
    o.srcmd5 = Some("8002a680d5ddba5efd407dae8a0b2835".into());
    o
}

#[test]
fn the_changed_added_and_removed_files_come_from_the_md5s() {
    let o = obs();
    let inputs = gather(&o, "1394662").unwrap();
    assert_eq!(o.called("diff"), 1);
    let a = &inputs.actions[0];
    assert_eq!(
        a.files.changed,
        [
            "0002-cracklib-2.9.2-visibility.patch",
            "cracklib.changes",
            "cracklib.spec"
        ]
    );
    assert_eq!(a.files.added, ["cracklib-2.10.4.tar.bz2"]);
    assert_eq!(a.files.removed, ["cracklib-2.10.3.tar.bz2"]);
    assert!(!a.target_missing);
    assert_eq!(a.new.spec(), Some(NEW_SPEC));
    assert_eq!(a.old.changes(), Some(OLD_CHANGES));
    assert_eq!(a.diff.as_deref(), Some("Index: cracklib.spec\n"));
    assert!(a.findings.is_empty(), "{:?}", a.findings);
}

#[test]
fn an_unchanged_file_is_read_once() {
    let o = obs();
    gather(&o, "1394662").unwrap();
    // 0001-cracklib-magic.diff has the same md5 on both sides: only the
    // target's read is kept for the source, which is not asked.
    let reads = o
        .calls
        .borrow()
        .iter()
        .filter(|c| c.contains("0001-cracklib-magic.diff"))
        .count();
    assert_eq!(reads, 1);
}

#[test]
fn a_target_that_is_not_there_gives_an_empty_base() {
    let mut o = obs();
    o.listings.remove(&(TP.into(), "cracklib".into(), None));
    let inputs = gather(&o, "1394662").unwrap();
    let a = &inputs.actions[0];
    assert!(a.target_missing);
    assert!(a.old.listing.files.is_empty());
    assert!(a.files.added.contains(&"cracklib.spec".to_string()));
}

#[test]
fn a_source_listing_failure_is_an_error() {
    let mut o = obs();
    o.listings
        .remove(&(SP.into(), "cracklib".into(), Some("3".into())));
    let e = gather(&o, "1394662").unwrap_err();
    assert!(e.0.contains("no longer exists"), "{e}");
    let mut o = obs();
    o.failing.insert(
        "listing",
        ReadFailure {
            what: "boom".into(),
            http: Some(503),
        },
    );
    assert!(gather(&o, "1394662").is_err());
}

#[test]
fn a_missing_request_and_an_unconfigured_osc_are_errors_with_their_cause() {
    let o = FakeRequestObs::default();
    assert_eq!(
        gather(&o, "7").unwrap_err(),
        ReviewError("request 7 does not exist".into())
    );
    let mut o = FakeRequestObs::default();
    o.failing.insert(
        "request",
        ReadFailure {
            what: "no".into(),
            http: Some(401),
        },
    );
    assert!(gather(&o, "7").unwrap_err().0.contains("not configured"));
}

#[test]
fn a_request_without_a_submit_action_is_an_error() {
    let mut o = FakeRequestObs::default();
    o.request.insert(
        "9".into(),
        "<request id=\"9\"><action type=\"delete\"><target project=\"a\" package=\"b\"/></action></request>".into(),
    );
    assert!(gather(&o, "9").unwrap_err().0.contains("no submit action"));
}

#[test]
fn a_diff_failure_is_incomplete_and_the_rest_still_runs() {
    let mut o = obs();
    o.failing.insert("diff", ReadFailure::new("timed out"));
    let inputs = gather(&o, "1394662").unwrap();
    let a = &inputs.actions[0];
    assert!(a.diff.is_none());
    assert_eq!(a.findings.len(), 1);
    assert_eq!(a.findings[0].id, ids::REVIEW_INCOMPLETE);
    assert_eq!(a.findings[0].severity, super::Severity::Note);
    assert!(a.findings[0].message.contains("timed out"));
    assert!(a.new.spec().is_some());
}

#[test]
fn the_other_open_requests_become_competing_findings() {
    let mut o = obs();
    o.open = crate::osc::parse_open_requests(include_str!(
        "../testdata/request_review/collection_competing.xml"
    ));
    let inputs = gather(&o, "1394662").unwrap();
    let ids: HashMap<_, _> = inputs.actions[0]
        .findings
        .iter()
        .map(|f| (f.id, 1))
        .collect();
    assert!(ids.contains_key(ids::REQUEST_COMPETING));
}

#[test]
fn a_text_over_the_size_cap_ends_the_review_instead_of_being_judged_missing() {
    let mut o = obs();
    let key = (
        SP.to_string(),
        "cracklib".to_string(),
        Some("3".to_string()),
    );
    let l = o.listings.get_mut(&key).unwrap();
    l.files
        .iter_mut()
        .find(|f| f.name == "cracklib.changes")
        .unwrap()
        .size = 9 << 20;
    let e = gather(&o, "1394662").unwrap_err();
    assert!(
        e.0.contains("cracklib.changes") && e.0.contains("cannot be completed"),
        "{e}"
    );
}

#[test]
fn a_text_that_cannot_be_read_ends_the_review() {
    let mut o = obs();
    o.files.remove(&(
        SP.into(),
        "cracklib".into(),
        "cracklib.changes".into(),
        Some("3".into()),
    ));
    let e = gather(&o, "1394662").unwrap_err();
    assert!(e.0.contains("cracklib.changes"), "{e}");
    let mut o = obs();
    o.files.remove(&(
        TP.into(),
        "cracklib".into(),
        "0002-cracklib-2.9.2-visibility.patch".into(),
        None,
    ));
    assert!(gather(&o, "1394662").is_err());
}
