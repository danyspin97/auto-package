use std::collections::BTreeMap;

use super::gather::Side;
use super::source_checks::source_findings;
use super::{Finding, Severity, ids};
use crate::osc::{ListedFile, Listing};

const SPEC: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: s\nLicense: MIT\n\
                    Source0: https://example.org/foo-%{version}.tar.gz\n\
                    Source1: foo-rpmlintrc\nPatch0: fix.patch\n";

fn side(spec: &str, names: &[&str], extra: &[(&str, &str)]) -> Side {
    let files = names
        .iter()
        .map(|n| ListedFile {
            name: n.to_string(),
            md5: "x".into(),
            size: 1,
        })
        .collect();
    let mut texts: BTreeMap<String, String> = extra
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    texts.insert("foo.spec".into(), spec.into());
    Side {
        listing: Listing {
            srcmd5: None,
            files,
        },
        texts,
        spec_name: Some("foo.spec".into()),
    }
}

const ALL: [&str; 5] = [
    "foo.spec",
    "foo-2.0.tar.gz",
    "foo-rpmlintrc",
    "fix.patch",
    "foo.changes",
];

fn run(base: &Side, new: &Side, added: &[&str]) -> Vec<Finding> {
    let added: Vec<String> = added.iter().map(|s| s.to_string()).collect();
    source_findings("foo", base, new, &added, &BTreeMap::new())
}

fn ids_of(f: &[Finding]) -> Vec<&'static str> {
    f.iter().map(|f| f.id).collect()
}

#[test]
fn a_package_with_all_its_files_is_quiet() {
    let s = side(SPEC, &ALL, &[]);
    assert!(run(&s, &s, &[]).is_empty());
}

#[test]
fn a_missing_source_is_a_blocker_and_a_warning_with_a_service() {
    let names = ["foo.spec", "foo-rpmlintrc", "fix.patch"];
    let base = side(SPEC, &ALL, &[]);
    let found = run(&base, &side(SPEC, &names, &[]), &[]);
    assert_eq!(ids_of(&found), [ids::SOURCE_MISSING]);
    assert_eq!(found[0].severity, Severity::Blocker);
    assert_eq!(found[0].file.as_deref(), Some("foo-2.0.tar.gz"));
    let with_service = ["foo.spec", "foo-rpmlintrc", "fix.patch", "_service"];
    let found = run(
        &base,
        &side(SPEC, &with_service, &[("_service", "<services/>")]),
        &["_service"],
    );
    assert_eq!(found[0].severity, Severity::Warning);
    // A file the service produced is listed with its prefix and counts.
    let produced = [
        "foo.spec",
        "foo-rpmlintrc",
        "fix.patch",
        "_service:download_url:foo-2.0.tar.gz",
    ];
    assert!(run(&base, &side(SPEC, &produced, &[]), &[]).is_empty());
}

#[test]
fn a_source_with_an_unknown_macro_is_a_note() {
    let spec = format!("{SPEC}Source5: %{{mystery}}.tar.xz\n");
    let s = side(&spec, &ALL, &[]);
    let base = side(SPEC, &ALL, &[]);
    assert_eq!(ids_of(&run(&base, &s, &[])), [ids::SOURCE_UNEXPANDED]);
    assert!(run(&s, &s, &[]).is_empty());
}

#[test]
fn an_archive_the_spec_dropped_and_the_package_kept_is_a_warning() {
    let old_spec = SPEC.replace("2.0", "1.0");
    let old_names = ["foo.spec", "foo-1.0.tar.gz", "foo-rpmlintrc", "fix.patch"];
    let base = side(&old_spec, &old_names, &[]);
    let kept = [
        "foo.spec",
        "foo-1.0.tar.gz",
        "foo-2.0.tar.gz",
        "foo-rpmlintrc",
        "fix.patch",
    ];
    let found = run(&base, &side(SPEC, &kept, &[]), &["foo-2.0.tar.gz"]);
    assert_eq!(ids_of(&found), [ids::SOURCE_OLD_ARCHIVE_KEPT]);
    assert_eq!(found[0].file.as_deref(), Some("foo-1.0.tar.gz"));
}

#[test]
fn leftovers_and_tool_artifacts_that_the_request_adds_are_warnings() {
    let mut names = ALL.to_vec();
    names.extend([
        "fix.patch.orig",
        "series-rebase",
        "series",
        "x.rej",
        ".#lock",
    ]);
    let base = side(SPEC, &ALL, &[]);
    let new = side(SPEC, &names, &[]);
    let found = run(
        &base,
        &new,
        &[
            "fix.patch.orig",
            "series-rebase",
            "series",
            "x.rej",
            ".#lock",
        ],
    );
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(found.iter().all(|f| f.id == ids::FILES_LEFTOVER));
    // `series` is not a leftover when the target had one.
    let mut with_series = ALL.to_vec();
    with_series.push("series");
    let base = side(SPEC, &with_series, &[]);
    let found = run(&base, &side(SPEC, &with_series, &[]), &["series"]);
    assert!(!ids_of(&found).contains(&ids::FILES_LEFTOVER));
}

#[test]
fn an_added_file_nobody_names_is_a_note_and_a_named_one_is_not() {
    let mut names = ALL.to_vec();
    names.extend(["stray.txt", "extra.keyring"]);
    let base = side(SPEC, &ALL, &[]);
    let found = run(
        &base,
        &side(SPEC, &names, &[]),
        &["stray.txt", "extra.keyring", "fix.patch"],
    );
    assert_eq!(ids_of(&found), [ids::FILES_UNREFERENCED]);
    assert_eq!(found[0].file.as_deref(), Some("stray.txt"));
}

#[test]
fn a_vendor_archive_with_a_target_directory_is_a_warning() {
    let s = side(SPEC, &ALL, &[]);
    let mut archives = BTreeMap::new();
    archives.insert(
        "vendor.tar.zst".to_string(),
        vec!["crate/a.rs".to_string(), "target/debug/x".to_string()],
    );
    archives.insert("foo-2.0.tar.gz".to_string(), vec!["target/x".to_string()]);
    let found = source_findings("foo", &s, &s, &[], &archives);
    assert_eq!(ids_of(&found), [ids::FILES_LEFTOVER]);
    assert_eq!(found[0].file.as_deref(), Some("vendor.tar.zst"));
}

#[test]
fn a_missing_source_under_a_conditional_is_only_a_warning() {
    let spec = SPEC.replace(
        "Source1: foo-rpmlintrc\n",
        "%if 0%{?suse_version} > 1600\nSource1: foo-rpmlintrc\n%endif\n",
    );
    let names = ["foo.spec", "foo-2.0.tar.gz", "fix.patch"];
    let base = side(SPEC, &ALL, &[]);
    let found = run(&base, &side(&spec, &names, &[]), &[]);
    assert_eq!(ids_of(&found), [ids::SOURCE_MISSING]);
    assert_eq!(found[0].severity, Severity::Warning);
    // The unconditional Source0 stays a blocker.
    let names = ["foo.spec", "fix.patch"];
    let found = run(&base, &side(&spec, &names, &[]), &[]);
    assert!(found.iter().any(|f| f.severity == Severity::Blocker));
}
