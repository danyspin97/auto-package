use super::*;
use crate::pipeline::header_problems;

const COMMIT: &str = include_str!("../testdata/bugfix/github_commit.patch");
const PLAIN: &str = include_str!("../testdata/bugfix/plain_fix.diff");
const PR: &str = include_str!("../testdata/bugfix/pr.patch");

fn refs() -> Vec<String> {
    vec!["bsc#1234567".into(), "CVE-2025-1234".into()]
}

fn facts(upstream: Option<&str>, status: UpstreamStatus) -> HeaderFacts {
    HeaderFacts {
        refs: refs(),
        upstream: upstream.map(str::to_string),
        status,
        reason: "check the token length".into(),
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn a_fix_is_named_after_the_package() {
    assert_eq!(
        fix_patch_name("curl", &refs(), Some("abcdef1234"), 0, 1),
        "curl-CVE-2025-1234.patch"
    );
    assert_eq!(
        fix_patch_name("curl", &refs(), Some("abcdef1234"), 0, 2),
        "curl-CVE-2025-1234-abcdef1.patch"
    );
    assert_eq!(
        fix_patch_name("curl", &s(&["bsc#1234567"]), Some("abcdef1234"), 0, 1),
        "curl-fix-abcdef1.patch"
    );
}

#[test]
fn a_prefix_the_package_already_uses_is_kept() {
    assert_eq!(patch_prefix("python-foo", &[]), "python-foo");
    assert_eq!(
        patch_prefix("python-foo", &s(&["foo-a.patch", "foo-b.patch"])),
        "foo"
    );
    assert_eq!(
        patch_prefix("foo", &s(&["foo-a.patch", "foo-b.patch"])),
        "foo"
    );
    // Kinds of patch are not a package convention.
    assert_eq!(
        patch_prefix("foo", &s(&["CVE-2020-1.patch", "CVE-2021-2.patch"])),
        "foo"
    );
    assert_eq!(patch_prefix("foo", &s(&["a.patch", "b.patch"])), "foo");
}

#[test]
fn a_format_patch_gets_its_lines_after_the_folded_subject() {
    let url = "https://github.com/o/r/commit/0123456";
    let out = with_fix_header(COMMIT.as_bytes(), &facts(Some(url), UpstreamStatus::Merged));
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(
            "cannot overflow the buffer\nReferences: bsc#1234567 CVE-2025-1234\nUpstream: merged https://github.com/o/r/commit/0123456\n\nThe copy"
        ),
        "{out}"
    );
    // Everything else is as it was.
    let drop = |t: &str| {
        t.lines()
            .filter(|l| !l.starts_with("References:") && !l.starts_with("Upstream:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(drop(&out), drop(COMMIT));
    assert!(
        header_problems(&out).is_empty(),
        "{:?}",
        header_problems(&out)
    );
}

#[test]
fn a_pull_request_is_submitted_and_only_its_first_mail_changes() {
    let url = "https://github.com/o/r/pull/7";
    let out = String::from_utf8(with_fix_header(
        PR.as_bytes(),
        &facts(Some(url), UpstreamStatus::Submitted),
    ))
    .unwrap();
    assert!(out.contains("Upstream: submitted https://github.com/o/r/pull/7\n"));
    assert_eq!(out.matches("Upstream:").count(), 1);
    assert!(header_problems(&out).is_empty());
}

#[test]
fn a_line_the_patch_has_is_not_doubled() {
    let url = "https://github.com/o/r/commit/0123456";
    let f = facts(Some(url), UpstreamStatus::Merged);
    let once = with_fix_header(COMMIT.as_bytes(), &f);
    let twice = with_fix_header(&once, &f);
    assert_eq!(once, twice);
}

#[test]
fn a_plain_diff_gets_a_header_block() {
    let url = "https://example.org/fix.diff";
    let out = String::from_utf8(with_fix_header(
        PLAIN.as_bytes(),
        &facts(Some(url), UpstreamStatus::Merged),
    ))
    .unwrap();
    assert!(out.starts_with(
        "Subject: check the token length\nReferences: bsc#1234567 CVE-2025-1234\nUpstream: merged https://example.org/fix.diff\n\nDescription: backport of https://example.org/fix.diff\n---\n--- a/src/parse.c"
    ), "{out}");
    assert!(out.ends_with(PLAIN));
    assert!(header_problems(&out).is_empty());
}

#[test]
fn a_patch_without_a_known_upstream_says_it_was_not_reported() {
    let out = String::from_utf8(with_fix_header(
        PLAIN.as_bytes(),
        &facts(None, UpstreamStatus::Merged),
    ))
    .unwrap();
    assert!(out.contains("Upstream: not yet reported\n"));
    assert!(header_problems(&out).is_empty());
}

#[test]
fn an_empty_commit_message_gets_a_description() {
    let mail = "From abc Mon Sep 17 00:00:00 2001\nFrom: A <a@b.c>\nSubject: [PATCH] x\n\n---\n a | 1 +\n\ndiff --git a/a b/a\n";
    let out = String::from_utf8(with_fix_header(
        mail.as_bytes(),
        &facts(
            Some("https://github.com/o/r/commit/abc1234"),
            UpstreamStatus::Merged,
        ),
    ))
    .unwrap();
    assert!(header_problems(&out).is_empty(), "{out}");
}

#[test]
fn bytes_that_are_not_utf8_are_kept() {
    let mut raw = PLAIN.as_bytes().to_vec();
    raw.extend_from_slice(b"+caf\xe9\n");
    let out = with_fix_header(&raw, &facts(None, UpstreamStatus::Merged));
    assert!(out.ends_with(&raw));
}

#[test]
fn text_for_a_spec_comment_has_no_macro_and_no_line_break() {
    assert_eq!(
        spec_safe("x %(touch /tmp/x) %{lua:os.exit()}"),
        "x %%(touch /tmp/x) %%{lua:os.exit()}"
    );
    assert_eq!(
        spec_safe("a\nPatch9: evil.patch\r\tb"),
        "a Patch9: evil.patch  b"
    );
    assert_eq!(spec_safe("plain text"), "plain text");
}

#[test]
fn subject_end_takes_the_folded_subject_lines() {
    let text = "From: a\nSubject: [PATCH] one\n two\nDate: x\n\nbody\n";
    let lines: Vec<&[u8]> = text.split_inclusive('\n').map(str::as_bytes).collect();
    let head = header_len(&lines);
    assert_eq!(subject_end(&lines, head), Some(3));
    let plain = "Index: x\n--- a\n+++ b\n";
    let lines: Vec<&[u8]> = plain.split_inclusive('\n').map(str::as_bytes).collect();
    assert_eq!(subject_end(&lines, header_len(&lines)), None);
}
