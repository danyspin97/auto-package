use super::*;

const SPEC: &str =
    "Name: fwupd-efi\nVersion: 1.9\nSource0: x.tar.gz\nSource1: fwupd-efi-rpmlintrc\n";

#[test]
fn the_rpmlintrc_the_spec_names_is_tidied_and_the_rest_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let text = "from Config import *\n\n# reason: r\naddFilter(\"a\")\n";
    std::fs::write(dir.path().join("fwupd-efi-rpmlintrc"), text).unwrap();
    // Not named by the spec: lint reports it, and it is never edited.
    std::fs::write(dir.path().join("other-rpmlintrc"), text).unwrap();
    let fixes = rpmlintrc_fixes(dir.path(), SPEC).unwrap();
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].name, "fwupd-efi-rpmlintrc");
    assert_eq!(fixes[0].before, text);
    assert_eq!(fixes[0].after, "# reason: r\naddFilter(\"a\")\n");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("a-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let clean = rpmlintrc_fixes(other.path(), "Name: a\nVersion: 1\nSource0: a.tar.gz\n");
    assert!(clean.unwrap().is_empty());
}

#[test]
fn a_named_rpmlintrc_the_checkout_lacks_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    assert!(rpmlintrc_fixes(dir.path(), SPEC).unwrap().is_empty());
}

#[test]
fn the_commit_says_what_it_changed() {
    assert_eq!(
        commit_message(true, false),
        "Tidy the spec (auto-package lint)"
    );
    assert_eq!(
        commit_message(false, true),
        "Add upstream provenance to patch headers (auto-package lint)"
    );
    assert_eq!(
        commit_message(true, true),
        "Tidy the spec (auto-package lint); Add upstream provenance to patch headers (auto-package lint)"
    );
}

#[test]
fn a_header_only_tidy_is_not_a_change_to_submit() {
    assert_eq!(tidy_done(false, false, true), TidyDone::ProvenanceOnly);
    assert_eq!(tidy_done(true, false, true), TidyDone::Changed);
    assert_eq!(tidy_done(false, true, false), TidyDone::Changed);
    assert_eq!(tidy_done(false, false, false), TidyDone::AlreadyTidy);
}

#[test]
fn only_a_header_only_tidy_is_skipped() {
    assert!(tidy_skip(TidyDone::ProvenanceOnly).is_some());
    assert_eq!(tidy_skip(TidyDone::AlreadyTidy), None);
    assert_eq!(tidy_skip(TidyDone::Changed), None);
}

#[test]
fn the_agent_is_skipped_only_for_items_with_fixable_findings() {
    use super::super::items::agent_needed;
    assert!(agent_needed(true, &["style: x".to_string()]));
    assert!(!agent_needed(true, &[]));
    assert!(!agent_needed(false, &["style: x".to_string()]));
}
