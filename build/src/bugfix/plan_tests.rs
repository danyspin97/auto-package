use super::*;
use crate::backport::PreparedPatch;

fn job() -> FixJob {
    FixJob {
        pkg: "foo".into(),
        refs: vec!["bsc#1234567".into(), "CVE-2025-1234".into()],
        description: None,
        bug_texts: vec![BugText {
            source: "bugzilla.suse.com bsc#1234567".into(),
            summary: "heap overflow".into(),
            comments: vec!["a".into(), "b".into()],
            ..Default::default()
        }],
        read: Vec::new(),
        not_read: Vec::new(),
        warnings: Vec::new(),
        upstream_repo: Some("https://github.com/o/r".into()),
        patches: vec![PreparedPatch {
            file_name: "foo-CVE-2025-1234.patch".into(),
            bytes: b"References: bsc#1234567 CVE-2025-1234\nUpstream: merged https://github.com/o/r/commit/abc1234\n\n---\n".to_vec(),
            origin: "https://github.com/o/r/commit/abc1234.patch".into(),
            commit: Some("abc1234".into()),
            reason: "check the length".into(),
        }],
        candidates: Vec::new(),
        found: Found::Given,
        unmerged: Vec::new(),
        fixed_in: FixedIn::NoneYet,
        fetch_hosts: Vec::new(),
        nonce: "n".into(),
    }
}

fn lines(job: &FixJob, submit: bool) -> String {
    dry_run_lines(job, "1.2.3", "home:u:branches:P", "P", submit).join("\n")
}

#[test]
fn a_given_commit_is_shown_with_its_header_comment_and_bullet() {
    let text = lines(&job(), false);
    for want in [
        "fix foo 1.2.3 in home:u:branches:P (the version stays 1.2.3):",
        "report: \"heap overflow\" (read from bugzilla.suse.com bsc#1234567, 2 comments kept)",
        "upstream repository: https://github.com/o/r",
        "upstream fix: https://github.com/o/r/commit/abc1234.patch (given)",
        "patch: foo-CVE-2025-1234.patch (4 lines, sha256 ",
        "header: References: bsc#1234567 CVE-2025-1234 / Upstream: merged https://github.com/o/r/commit/abc1234",
        "spec:   # PATCH-FIX-UPSTREAM foo-CVE-2025-1234.patch bsc#1234567 CVE-2025-1234 -- check the length",
        ".changes: - Add foo-CVE-2025-1234.patch: check the length (bsc#1234567, CVE-2025-1234)",
        "upstream release with the fix: none yet",
        "submit: no (--submit not given)",
    ] {
        assert!(text.contains(want), "{want}\n{text}");
    }
}

#[test]
fn several_candidates_are_listed_and_a_missing_fix_says_so() {
    let mut j = job();
    j.patches.clear();
    j.candidates = vec![
        "https://github.com/o/r/commit/a.patch".into(),
        "https://github.com/o/r/pull/3.patch".into(),
    ];
    let text = lines(&j, false);
    assert!(
        text.contains("several candidates, none chosen (pass the ones to use with --commit-url):"),
        "{text}"
    );
    assert!(text.contains("     https://github.com/o/r/pull/3.patch"));
    j.candidates.clear();
    assert!(
        lines(&j, false)
            .contains("none found: the agent looks in the repository, else writes the fix")
    );
}

#[test]
fn a_report_that_was_not_read_says_why_and_text_given_is_marked() {
    let mut j = job();
    j.bug_texts.clear();
    j.not_read = vec!["bsc#1234567: bugzilla.suse.com is not an allowed host (auto-package hosts allow bugzilla.suse.com, --allow-host bugzilla.suse.com, or pass --bug-file)".into()];
    j.description = Some("it crashes".into());
    let text = lines(&j, false);
    assert!(
        text.contains("not read: bsc#1234567: bugzilla.suse.com is not an allowed host"),
        "{text}"
    );
    assert!(text.contains("report: given as text (--description)"));
}

#[test]
fn a_fixed_release_suggests_the_update_and_submit_names_the_origin() {
    let mut j = job();
    j.fixed_in = FixedIn::Tag("v1.2.4".into());
    let text = lines(&j, true);
    assert!(text.contains("v1.2.4 (`auto-package update --project <P> --only foo --to-version 1.2.4` would take it instead)"), "{text}");
    assert!(text.contains("submit: a request to P after confirmation"));
    j.fixed_in = FixedIn::NotChecked("only GitHub repositories are compared".into());
    assert!(lines(&j, false).contains("not checked: only GitHub"));
}

#[test]
fn an_unmerged_fix_is_shown_with_a_warning() {
    let mut j = job();
    assert!(!lines(&j, false).contains("not shown to be merged"));
    j.unmerged = vec!["foo-CVE-2025-1234.patch".into()];
    assert!(lines(&j, false).contains("warning: not shown to be merged upstream"));
}
