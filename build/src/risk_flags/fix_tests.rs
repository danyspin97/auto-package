use super::*;

const HEADER: &str = "Subject: s\nUpstream: merged https://github.com/o/r/commit/abc1234\n\n---\n";
const FETCHED: &str = "--- a/f.c\n+++ b/f.c\n@@ -10,3 +10,4 @@\n a\n+b\n c\n d\n";

fn files(patches: &[(&str, String)]) -> Files {
    Files {
        spec: None,
        service: None,
        patches: patches
            .iter()
            .map(|(n, t)| (n.to_string(), t.clone()))
            .collect(),
    }
}

fn staged() -> Vec<(String, String)> {
    vec![("foo-fix.patch".into(), format!("{HEADER}{FETCHED}"))]
}

#[test]
fn a_patch_that_says_not_yet_reported_is_a_fix_without_upstream() {
    let text = "Subject: s\nUpstream: Not yet reported\n\n---\n--- a/f.c\n+++ b/f.c\n@@ -1 +1 @@\n-a\n+b\n";
    let flags = bug_fix_risks(
        &[],
        &[],
        &files(&[]),
        &files(&[("foo-fix.patch", text.into())]),
    );
    assert_eq!(
        flags,
        vec![RiskFlag::FixWithoutUpstream {
            patch: "foo-fix.patch".into()
        }]
    );
    let line = flags[0].line();
    assert!(
        line.contains("foo-fix.patch") && line.contains("without an upstream commit"),
        "{line}"
    );
}

#[test]
fn an_upstream_patch_with_a_url_is_not_flagged_and_an_old_patch_is_ignored() {
    let after = files(&[("foo-fix.patch", format!("{HEADER}{FETCHED}"))]);
    assert!(bug_fix_risks(&staged(), &[], &files(&[]), &after).is_empty());
    let unreported = "Upstream: not yet reported\n@@ -1 +1 @@\n-a\n+b\n".to_string();
    let both = files(&[("old.patch", unreported)]);
    assert!(bug_fix_risks(&[], &[], &both, &both).is_empty());
}

#[test]
fn a_patch_refreshed_only_by_offset_and_context_is_not_adapted() {
    let refreshed = format!("{HEADER}--- a/f.c\n+++ b/f.c\n@@ -42,3 +42,4 @@\n a\n+b\n x\n d\n");
    let flags = bug_fix_risks(
        &staged(),
        &[],
        &files(&[]),
        &files(&[("foo-fix.patch", refreshed)]),
    );
    assert!(flags.is_empty(), "{flags:?}");
}

#[test]
fn a_changed_code_line_is_an_adapted_upstream_fix() {
    let adapted = format!("{HEADER}--- a/f.c\n+++ b/f.c\n@@ -10,3 +10,4 @@\n a\n+bb\n c\n d\n");
    let flags = bug_fix_risks(
        &staged(),
        &[],
        &files(&[]),
        &files(&[("foo-fix.patch", adapted)]),
    );
    assert_eq!(
        flags,
        vec![RiskFlag::AdaptedUpstreamFix {
            patch: "foo-fix.patch".into()
        }]
    );
    assert!(flags[0].line().contains("needs") || flags[0].line().contains("checks"));
}

#[test]
fn code_lines_read_hunks_by_their_counts() {
    // A removed line that starts with `--` is code, not a file header.
    let text = "--- a/x.sql\n+++ b/x.sql\n@@ -1,2 +1,2 @@\n--- comment\n+++ comment\n same\n";
    assert_eq!(code_lines(text), ["--- comment", "+++ comment"]);
    assert!(code_lines("no hunks here").is_empty());
}

#[test]
fn a_patch_the_agent_wrote_is_flagged_whatever_its_header_claims() {
    // It cites a merged upstream commit, but the tool did not stage it.
    let claims = format!("{HEADER}{FETCHED}");
    let flags = bug_fix_risks(&[], &[], &files(&[]), &files(&[("foo-fix.patch", claims)]));
    assert_eq!(
        flags,
        vec![RiskFlag::FixWithoutUpstream {
            patch: "foo-fix.patch".into()
        }]
    );
}

#[test]
fn a_staged_request_or_unverified_commit_is_flagged_unmerged() {
    let after = files(&[("foo-fix.patch", format!("{HEADER}{FETCHED}"))]);
    let flags = bug_fix_risks(
        &staged(),
        &["foo-fix.patch".to_string()],
        &files(&[]),
        &after,
    );
    assert_eq!(
        flags,
        vec![RiskFlag::UnmergedUpstreamFix {
            patch: "foo-fix.patch".into()
        }]
    );
    assert!(flags[0].line().contains("not shown to be merged"));
}
