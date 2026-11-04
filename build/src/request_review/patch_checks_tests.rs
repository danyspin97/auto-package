use std::collections::BTreeMap;

use super::gather::Side;
use super::patch_checks::patch_findings;
use super::{Finding, Severity, ids};

const OLD_SPEC: &str = include_str!("../testdata/request_review/cracklib_old.spec");
const NEW_SPEC: &str = include_str!("../testdata/request_review/cracklib_new.spec");
const OLD_PATCH: &str = include_str!("../testdata/request_review/visibility_old.patch");
const NEW_PATCH: &str = include_str!("../testdata/request_review/visibility_new.patch");
const VIS: &str = "0002-cracklib-2.9.2-visibility.patch";

fn side(spec: &str, files: &[(&str, &str)]) -> Side {
    let mut texts: BTreeMap<String, String> = files
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    texts.insert("cracklib.spec".into(), spec.into());
    Side {
        listing: Default::default(),
        texts,
        spec_name: Some("cracklib.spec".into()),
    }
}

fn ids_of(f: &[Finding]) -> Vec<&'static str> {
    f.iter().map(|f| f.id).collect()
}

const GOOD_HEADER: &str = "Subject: Fix the thing\nDescription: why\nUpstream: not yet reported\n";

/// The spec with a `Patch9:` line added, with or without its comment.
fn with_patch(comment: bool, name: &str) -> String {
    let line = format!("Patch9:          {name}\n");
    let line = if comment {
        format!("# PATCH-FIX-OPENSUSE {name} -- why\n{line}")
    } else {
        line
    };
    NEW_SPEC.replacen("Source2:", &format!("{line}Source2:"), 1)
}

#[test]
fn the_refreshed_visibility_patch_of_the_real_request_is_quiet() {
    let base = side(OLD_SPEC, &[(VIS, OLD_PATCH)]);
    let new = side(NEW_SPEC, &[(VIS, NEW_PATCH)]);
    let found = patch_findings("cracklib", &base, &new);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_new_patch_without_header_or_comment_is_two_warnings() {
    let base = side(OLD_SPEC, &[(VIS, OLD_PATCH)]);
    let body = "--- a.c.orig\n+++ a.c\n@@ -1 +1 @@\n-a\n+b\n";
    let new = side(
        &with_patch(false, "fix.patch"),
        &[(VIS, OLD_PATCH), ("fix.patch", body)],
    );
    let found = patch_findings("cracklib", &base, &new);
    let ids = ids_of(&found);
    assert!(ids.contains(&ids::PATCH_COMMENT), "{found:?}");
    assert!(ids.contains(&ids::PATCH_HEADER), "{found:?}");
    assert!(found.iter().all(|f| f.severity == Severity::Warning));
}

#[test]
fn a_new_patch_with_header_and_comment_is_quiet_and_a_version_in_its_name_is_not() {
    let base = side(OLD_SPEC, &[(VIS, OLD_PATCH)]);
    let body = format!("{GOOD_HEADER}--- a.c.orig\n+++ a.c\n@@ -1 +1 @@\n-a\n+b\n");
    let new = side(
        &with_patch(true, "fix.patch"),
        &[(VIS, OLD_PATCH), ("fix.patch", &body)],
    );
    assert!(patch_findings("cracklib", &base, &new).is_empty());
    let named = side(
        &with_patch(true, "fix-%{version}.patch"),
        &[(VIS, OLD_PATCH)],
    );
    let found = patch_findings("cracklib", &base, &named);
    assert!(
        ids_of(&found).contains(&ids::PATCH_VERSION_IN_NAME),
        "{found:?}"
    );
}

#[test]
fn a_changed_patch_keeps_quiet_about_a_header_it_never_had_but_not_about_a_lost_one() {
    let headed = format!("{GOOD_HEADER}{OLD_PATCH}");
    let base = side(OLD_SPEC, &[(VIS, &headed)]);
    let new = side(OLD_SPEC, &[(VIS, NEW_PATCH)]);
    let found = patch_findings("cracklib", &base, &new);
    assert!(ids_of(&found).contains(&ids::PATCH_HEADER), "{found:?}");
    assert!(found[0].message.contains("(changed)"));
}

#[test]
fn a_patch_the_spec_stops_naming_is_a_note() {
    let base = side(OLD_SPEC, &[(VIS, OLD_PATCH)]);
    let without = NEW_SPEC
        .lines()
        .filter(|l| !l.starts_with("Patch1"))
        .collect::<Vec<_>>()
        .join("\n");
    let new = side(&without, &[]);
    let found = patch_findings("cracklib", &base, &new);
    let dropped: Vec<_> = found
        .iter()
        .filter(|f| f.id == ids::PATCH_DROPPED)
        .collect();
    assert_eq!(dropped.len(), 1, "{found:?}");
    assert_eq!(dropped[0].severity, Severity::Note);
}

#[test]
fn a_patch_applied_from_prep_is_a_spec_route_warning() {
    let base = side(OLD_SPEC, &[]);
    let routed = NEW_SPEC.replacen("%prep\n", "%prep\npatch -p1 < %{SOURCE5}\n", 1);
    let new = side(&routed, &[]);
    let found = patch_findings("cracklib", &base, &new);
    assert!(ids_of(&found).contains(&ids::PATCH_SPEC_ROUTE), "{found:?}");
}

fn rc(text: &str, old: &str) -> Vec<Finding> {
    let base = side(OLD_SPEC, &[("p-rpmlintrc", old)]);
    let new = side(OLD_SPEC, &[("p-rpmlintrc", text)]);
    patch_findings("cracklib", &base, &new)
}

#[test]
fn rpmlintrc_rules_are_judged_on_the_added_lines() {
    let found = rc("setBadness('x', 0)\n", "");
    assert_eq!(ids_of(&found), [ids::RPMLINTRC_SET_BADNESS]);
    assert_eq!(found[0].severity, Severity::Blocker);
    let found = rc("from Config import *\n", "");
    assert_eq!(ids_of(&found), [ids::RPMLINTRC_NON_FILTER]);
    let found = rc("addFilter(\"zero-length.*\")\n", "");
    assert_eq!(ids_of(&found), [ids::RPMLINTRC_NO_REASON]);
    assert!(rc("# reason: vendor file\naddFilter(\"zero-length.*\")\n", "").is_empty());
    // A line the old file already had is not the request's.
    assert!(
        rc(
            "addFilter(\"zero-length.*\")\n",
            "addFilter(\"zero-length.*\")\n"
        )
        .is_empty()
    );
}
