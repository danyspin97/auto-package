use super::downloads::{REVIEW_DOWNLOAD_MAX_BYTES, download_disabled, download_sources};
use super::fetch_fake::FakeRequestObs;
use super::gather::{ActionInputs, gather};
use super::signature_checks::{keyring_findings, signature_result};
use super::{Severity, ids};
use crate::osc::{ListedFile, Listing};
use crate::pipeline::SignatureCheck;

const SPEC: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: s\nLicense: MIT\n\
                    Source0: https://example.org/foo-%{version}.tar.gz\n\
                    Source1: foo-%{version}.tar.gz.asc\nSource2: foo.keyring\n";

fn listing(size: u64, md5: &str) -> Listing {
    let file = |name: &str, size: u64, md5: &str| ListedFile {
        name: name.into(),
        md5: md5.into(),
        size,
    };
    Listing {
        srcmd5: Some("abc".into()),
        files: vec![
            file("foo.spec", 10, "s"),
            file("foo-2.0.tar.gz", size, md5),
            file("foo-2.0.tar.gz.asc", 1, md5),
            file("foo.keyring", 1, "k"),
        ],
    }
}

fn inputs(size: u64) -> (FakeRequestObs, ActionInputs) {
    let mut o = FakeRequestObs::default();
    o.request.insert(
        "5".into(),
        "<request id=\"5\"><action type=\"submit\"><source project=\"home:u\" package=\"foo\" rev=\"1\"/>\
         <target project=\"Base\" package=\"foo\"/></action>\
         <state name=\"new\" who=\"u\" when=\"2026-01-01T00:00:00\"/></request>"
            .into(),
    );
    let key =
        |p: &str, rev: Option<&str>| (p.to_string(), "foo".to_string(), rev.map(String::from));
    o.listings
        .insert(key("home:u", Some("1")), listing(size, "new"));
    o.listings.insert(key("Base", None), listing(size, "old"));
    for (p, rev) in [("home:u", Some("1")), ("Base", None)] {
        o.files.insert(
            (
                p.into(),
                "foo".into(),
                "foo.spec".into(),
                rev.map(String::from),
            ),
            SPEC.into(),
        );
    }
    o.srcmd5 = Some("abc".into());
    o.diff = Some(String::new());
    let mut got = gather(&o, "5").unwrap();
    (o, got.actions.remove(0))
}

#[test]
fn a_signed_archive_comes_with_its_signature_keyring_and_is_the_main_archive() {
    let (o, a) = inputs(100);
    let dir = tempfile::tempdir().unwrap();
    let d = download_sources(&o, &a, dir.path());
    assert_eq!(
        d.fetched,
        ["foo-2.0.tar.gz", "foo-2.0.tar.gz.asc", "foo.keyring"]
    );
    assert!(dir.path().join("foo.keyring").is_file());
    assert_eq!(d.main_archive, Some(dir.path().join("foo-2.0.tar.gz")));
    assert!(d.findings.is_empty() && d.skipped.is_empty());
    assert!(!dir.path().join(".fetch").exists());
}

#[test]
fn a_file_that_would_pass_the_cap_is_skipped_with_its_size() {
    let (o, a) = inputs(REVIEW_DOWNLOAD_MAX_BYTES + 1);
    let dir = tempfile::tempdir().unwrap();
    let d = download_sources(&o, &a, dir.path());
    assert!(!d.fetched.contains(&"foo-2.0.tar.gz".to_string()));
    assert!(
        d.skipped
            .iter()
            .any(|s| s.check == "foo-2.0.tar.gz" && s.why.contains("cap"))
    );
    assert_eq!(d.main_archive, None);
}

#[test]
fn a_failed_download_of_a_signed_file_is_a_required_failure() {
    let (mut o, a) = inputs(100);
    o.failing
        .insert("download", super::fetch::ReadFailure::new("timed out"));
    let dir = tempfile::tempdir().unwrap();
    let d = download_sources(&o, &a, dir.path());
    assert!(d.fetched.is_empty());
    let why = d.required_failure.unwrap();
    assert!(
        why.contains("foo-2.0.tar.gz") && why.contains("timed out"),
        "{why}"
    );
}

#[test]
fn a_failed_download_of_only_the_main_archive_is_a_note() {
    let (mut o, a) = inputs(100);
    // No signature in the package: the archive is only the main one.
    for l in o.listings.values_mut() {
        l.files
            .retain(|f| f.name != "foo-2.0.tar.gz.asc" && f.name != "foo.keyring");
    }
    let mut a = a;
    a.new
        .listing
        .files
        .retain(|f| f.name != "foo-2.0.tar.gz.asc" && f.name != "foo.keyring");
    o.failing
        .insert("download", super::fetch::ReadFailure::new("timed out"));
    let dir = tempfile::tempdir().unwrap();
    let d = download_sources(&o, &a, dir.path());
    assert_eq!(d.required_failure, None);
    assert_eq!(d.findings.len(), 1);
    assert_eq!(d.findings[0].id, ids::REVIEW_INCOMPLETE);
    assert_eq!(d.findings[0].severity, Severity::Note);
}

#[test]
fn the_disabled_downloads_say_what_was_not_checked() {
    let (_, a) = inputs(100);
    let said = download_disabled(&a);
    assert!(said.iter().any(|s| s.check == "signatures"));
    assert!(said.iter().any(|s| s.check == "source archive"));
}

#[test]
fn a_signature_result_maps_to_its_id_and_never_mentions_obs_being_sent_to() {
    assert_eq!(signature_result("foo", SignatureCheck::Ok), None);
    let f = signature_result(
        "foo",
        SignatureCheck::Refused("bad. Nothing was sent to OBS".into()),
    )
    .unwrap();
    assert_eq!(
        (f.id, f.severity),
        (ids::SOURCE_SIGNATURE, Severity::Blocker)
    );
    assert!(!f.message.contains("sent to OBS"));
    let f = signature_result("foo", SignatureCheck::Skipped("no gpg".into())).unwrap();
    assert_eq!(
        (f.id, f.severity),
        (ids::SOURCE_SIGNATURE_NOT_CHECKED, Severity::Note)
    );
}

#[test]
fn a_keyring_the_request_changes_is_a_warning() {
    let (_, a) = inputs(100);
    assert!(keyring_findings(&a).is_empty());
    let mut changed = a.clone();
    changed.files.changed.push("foo.keyring".into());
    let found = keyring_findings(&changed);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, ids::SOURCE_KEYRING_CHANGED);
}
