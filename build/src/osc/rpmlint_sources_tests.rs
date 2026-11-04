use super::*;

const SPEC: &str = "Name: fwupd-efi\nVersion: 1.9\nSource: https://example.org/fwupd-%{version}.tar.gz\nSource1: LICENSE\nPatch0: 106.patch\n";

/// A spec with a Source line the rpmlintrc name follows: `Source2`.
#[test]
fn a_filter_gets_the_next_source_number_after_the_last_source() {
    let out = with_rpmlintrc_source(SPEC, "fwupd-efi-rpmlintrc").unwrap();
    assert!(
        out.contains("Source1: LICENSE\nSource2:        fwupd-efi-rpmlintrc\nPatch0"),
        "{out}"
    );
    assert_eq!(out.matches("Source2").count(), 1);
}

/// No Source line at all: there is no place to name the filter.
#[test]
fn a_spec_without_a_source_line_is_not_edited() {
    assert_eq!(with_rpmlintrc_source("Name: x\n", "x-rpmlintrc"), None);
}

/// A last Source line without a newline still ends its line before the new one.
#[test]
fn a_last_source_line_without_a_newline_is_closed() {
    let out = with_rpmlintrc_source("Name: x\nSource0: x.tar.gz", "x-rpmlintrc").unwrap();
    assert_eq!(
        out,
        "Name: x\nSource0: x.tar.gz\nSource1:        x-rpmlintrc\n"
    );
}

/// A filter this run wrote is named in the spec that builds it.
#[test]
fn a_written_filter_is_named_in_its_spec() {
    let dir = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fwupd-efi.spec"), SPEC).unwrap();
    fs::write(
        dir.path().join("fwupd-efi-rpmlintrc"),
        "addFilter(\"no-binary\")\n",
    )
    .unwrap();
    assert_eq!(unnamed_rpmlintrcs(dir.path()), ["fwupd-efi-rpmlintrc"]);
    note_rpmlintrc_written(run.path(), "fwupd-efi-rpmlintrc");
    let written = rpmlintrcs_written(run.path());
    let (added, left_out) = name_unnamed_rpmlintrcs(dir.path(), &written).unwrap();
    assert_eq!(added, ["fwupd-efi-rpmlintrc"]);
    assert!(left_out.is_empty());
    let spec = fs::read_to_string(dir.path().join("fwupd-efi.spec")).unwrap();
    assert!(
        spec.contains("Source2:        fwupd-efi-rpmlintrc"),
        "{spec}"
    );
    assert!(unnamed_rpmlintrcs(dir.path()).is_empty());
}

/// A hand-placed filter this run did not write stays out, and is reported.
#[test]
fn a_file_this_run_did_not_write_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fwupd-efi.spec"), SPEC).unwrap();
    fs::write(dir.path().join("fwupd-efi-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let (added, left_out) = name_unnamed_rpmlintrcs(dir.path(), &[]).unwrap();
    assert!(added.is_empty());
    assert_eq!(left_out, ["fwupd-efi-rpmlintrc"]);
    assert_eq!(
        fs::read_to_string(dir.path().join("fwupd-efi.spec")).unwrap(),
        SPEC
    );
}

/// An agent-written dotted filter the run accepted is named the same way.
#[test]
fn an_accepted_dotted_filter_is_named() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fwupd-efi.spec"), SPEC).unwrap();
    fs::write(dir.path().join("fwupd-efi.rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let written = ["fwupd-efi.rpmlintrc".to_string()];
    let (added, _) = name_unnamed_rpmlintrcs(dir.path(), &written).unwrap();
    assert_eq!(added, ["fwupd-efi.rpmlintrc"]);
}

/// An empty filter is dropped by the commit on purpose: it is not unnamed.
#[test]
fn an_empty_filter_is_not_reported() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fwupd-efi.spec"), SPEC).unwrap();
    fs::write(dir.path().join("fwupd-efi-rpmlintrc"), "").unwrap();
    assert!(unnamed_rpmlintrcs(dir.path()).is_empty());
}

/// A filter of a package whose spec is a sibling is named in that sibling spec.
#[test]
fn a_filter_is_named_in_the_spec_of_its_own_prefix() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("elfutils-debuginfod.spec"),
        "Name: elfutils-debuginfod\nSource0: a.tar.gz\n",
    )
    .unwrap();
    fs::write(dir.path().join("elfutils.spec"), SPEC).unwrap();
    fs::write(dir.path().join("elfutils-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let (added, _) =
        name_unnamed_rpmlintrcs(dir.path(), &["elfutils-rpmlintrc".to_string()]).unwrap();
    assert_eq!(added, ["elfutils-rpmlintrc"]);
    let spec = fs::read_to_string(dir.path().join("elfutils.spec")).unwrap();
    assert!(
        spec.contains("Source2:        elfutils-rpmlintrc"),
        "{spec}"
    );
}

/// A filter with no spec of its prefix is refused with the line to add, and no
/// spec changes, even for a name that was fine before it in the same run.
#[test]
fn a_refusal_names_the_fix_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("fwupd-efi.spec"), SPEC).unwrap();
    fs::write(dir.path().join("fwupd-efi-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    fs::write(dir.path().join("other-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let written = [
        "fwupd-efi-rpmlintrc".to_string(),
        "other-rpmlintrc".to_string(),
    ];
    let why = name_unnamed_rpmlintrcs(dir.path(), &written).unwrap_err();
    assert!(why.contains("other-rpmlintrc"), "{why}");
    assert!(
        why.contains("add a line `Source<N>: other-rpmlintrc`"),
        "{why}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fwupd-efi.spec")).unwrap(),
        SPEC
    );
}

/// The changelog claims a filter; nothing published carries one: refused.
#[test]
fn a_filter_claim_without_a_published_filter_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("fwupd-efi.changes"),
        "-------------------------------------------------------------------\n\
         Mon Nov 2 2026 - x\n\n- Filter the no-binary rpmlint error for the UEFI applications\n\n\
         -------------------------------------------------------------------\n\
         Sun Oct 1 2026 - x\n\n- Older entry\n",
    )
    .unwrap();
    let published = [
        "fwupd-efi.changes".to_string(),
        "fwupd-efi.spec".to_string(),
    ];
    assert!(filter_claim_without_rpmlintrc(dir.path(), &published).is_some());
    let with = [published[0].clone(), "fwupd-efi-rpmlintrc".to_string()];
    assert!(filter_claim_without_rpmlintrc(dir.path(), &with).is_none());
}

/// A changelog that mentions no filter claims nothing.
#[test]
fn a_changelog_without_a_filter_claim_is_not_flagged() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("fwupd-efi.changes"),
        "-------------------------------------------------------------------\n\
         Mon Nov 2 2026 - x\n\n- Bump to 1.9\n\n\
         -------------------------------------------------------------------\n",
    )
    .unwrap();
    assert!(filter_claim_without_rpmlintrc(dir.path(), &[]).is_none());
}
