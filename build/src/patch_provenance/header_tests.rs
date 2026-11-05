use super::header::checked_insert;
use super::*;
use crate::bugfix::header_len;
use crate::patch_provenance::{OriginEvidence, OriginKind, PatchOrigin};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn pr_origin() -> PatchOrigin {
    PatchOrigin {
        url: "https://github.com/fwupd/fwupd-efi/pull/106".into(),
        kind: OriginKind::Pr,
        project: "https://github.com/fwupd/fwupd-efi".into(),
        number: Some(106),
        commit: None,
        evidence: OriginEvidence::ChangesBullet,
    }
}

fn claim<'a>(
    origin: &'a PatchOrigin,
    status: Option<&'static str>,
    reason: Option<&str>,
) -> Claim<'a> {
    Claim {
        origin,
        status,
        commit: None,
        reason: reason.map(str::to_string),
    }
}

const MAIL: &str = "From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001\n\
From: Some One <some@one.example>\n\
Date: Tue, 1 Jul 2025 10:00:00 +0200\n\
Subject: [PATCH 1/4] ARM32 crt0: Split headers\n\
\x20and code\n\
\n\
The commit message body.\n\
---\n\
 crt0.S | 2 +-\n\
--- a/crt0.S\n\
+++ b/crt0.S\n\
@@ -1 +1 @@\n\
-a\n\
+b\n";

#[test]
fn a_mail_gets_its_references_and_upstream_after_the_folded_subject() {
    let o = pr_origin();
    let out = with_header(MAIL, &claim(&o, Some("submitted"), Some("ignored"))).unwrap();
    let expected = MAIL.replace(
        " and code\n",
        " and code\nReferences: gh#fwupd/fwupd-efi#106\nUpstream: submitted https://github.com/fwupd/fwupd-efi/pull/106\n",
    );
    assert_eq!(out, expected);
}

#[test]
fn a_header_with_upstream_is_left_alone() {
    let o = pr_origin();
    let text = MAIL.replace("Subject:", "Upstream: merged https://x.example/a\nSubject:");
    assert_eq!(
        with_header(&text, &claim(&o, Some("submitted"), None)),
        None
    );
}

#[test]
fn only_the_missing_references_line_is_kept_out() {
    let o = pr_origin();
    let text = MAIL.replace("Subject:", "References: bsc#1234\nSubject:");
    let out = with_header(&text, &claim(&o, Some("merged"), None)).unwrap();
    assert!(out.contains("References: bsc#1234\n"));
    assert_eq!(out.matches("References:").count(), 1);
    assert!(out.contains("Upstream: merged https://github.com/fwupd/fwupd-efi/pull/106\n"));
}

#[test]
fn a_second_run_changes_nothing() {
    let o = pr_origin();
    let once = with_header(MAIL, &claim(&o, Some("submitted"), None)).unwrap();
    assert_eq!(
        with_header(&once, &claim(&o, Some("submitted"), None)),
        None
    );
}

#[test]
fn a_plain_file_gets_the_lines_at_the_top_then_a_blank_line() {
    let o = pr_origin();
    let plain = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let out = with_header(plain, &claim(&o, None, None)).unwrap();
    assert_eq!(
        out,
        format!(
            "References: gh#fwupd/fwupd-efi#106\nUpstream: https://github.com/fwupd/fwupd-efi/pull/106\n\n{plain}"
        )
    );
}

#[test]
fn a_patch_that_starts_with_an_mbox_line_and_no_subject_is_not_touched() {
    let o = pr_origin();
    let text = format!("From {SHA} Mon Sep 17 00:00:00 2001\n\n--- a/x\n");
    assert_eq!(
        with_header(&text, &claim(&o, Some("submitted"), None)),
        None
    );
}

#[test]
fn a_description_is_written_only_when_the_header_has_none() {
    let o = pr_origin();
    let bare = "Subject: [PATCH] x\n\n---\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let out = with_header(
        bare,
        &claim(&o, Some("submitted"), Some("Fixes the crt0 split")),
    )
    .unwrap();
    assert!(out.contains("Description: Fixes the crt0 split\n"), "{out}");
    assert!(with_header(MAIL, &claim(&o, Some("submitted"), Some("x"))).is_some());
    let out = with_header(MAIL, &claim(&o, Some("submitted"), Some("x"))).unwrap();
    assert!(!out.contains("Description:"), "{out}");
}

#[test]
fn the_git_commit_line_is_added_for_a_commit() {
    let o = PatchOrigin {
        url: format!("https://github.com/o/r/commit/{SHA}"),
        kind: OriginKind::Commit,
        project: "https://github.com/o/r".into(),
        number: None,
        commit: Some(SHA.into()),
        evidence: OriginEvidence::PatchHeader,
    };
    let c = Claim {
        origin: &o,
        status: None,
        commit: Some(SHA.into()),
        reason: None,
    };
    let text = "Subject: x\n\n---\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let out = with_header(text, &c).unwrap();
    assert!(out.contains(&format!("Git-commit: {SHA}\n")));
    assert!(out.contains(&format!("Upstream: https://github.com/o/r/commit/{SHA}\n")));
    assert!(!out.contains("References:"));
}

#[test]
fn a_description_folds_at_76_columns_without_splitting_a_word() {
    let value = "Improves ARM32 crt0 by splitting header and code; Use pkgconfig for gnu-efi - fixes RISC-V build";
    let lines = fold_field("Description", value);
    assert_eq!(
        lines[0],
        "Description: Improves ARM32 crt0 by splitting header and code; Use pkgconfig"
    );
    assert_eq!(lines[1], " for gnu-efi - fixes RISC-V build");
    assert!(lines.iter().all(|l| l.len() <= 76), "{lines:?}");
    assert!(lines[1..].iter().all(|l| l.starts_with(' ')));
    let joined: Vec<&str> = lines.iter().flat_map(|l| l.split_whitespace()).collect();
    let expect: Vec<&str> = std::iter::once("Description:")
        .chain(value.split_whitespace())
        .collect();
    assert_eq!(joined, expect);
}

#[test]
fn a_reason_is_cut_at_200_characters_on_a_word_boundary() {
    let long = "word ".repeat(80);
    let s = sanitize(&long);
    assert!(s.chars().count() <= 200, "{}", s.len());
    assert!(s.ends_with("word"));
}

#[test]
fn control_and_bidi_characters_become_spaces() {
    assert_eq!(sanitize("a\u{202E}b\tc\u{7}d."), "a b c d");
}

#[test]
fn a_percent_is_kept_in_the_header_and_doubled_in_the_spec_comment() {
    let o = pr_origin();
    let out = with_header(
        "Subject: x\n\n---\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
        &claim(&o, Some("submitted"), Some("Fix 100% of cases")),
    )
    .unwrap();
    assert!(out.contains("Description: Fix 100% of cases\n"), "{out}");
    let spec = "Name: x\nPatch0: 106.patch\n";
    let out = spec_with_comment(
        spec,
        "106.patch",
        "gh#fwupd/fwupd-efi#106",
        Some("Fix 100% of cases"),
    )
    .unwrap();
    assert!(out.contains("-- Fix 100%% of cases"), "{out}");
}

#[test]
fn the_spec_comment_goes_above_the_patch_line_under_free_text() {
    let spec = "Name: x\n# some free text\nPatch0: 106.patch\n";
    let out = spec_with_comment(spec, "106.patch", "gh#o/r#106", Some("Fixes it")).unwrap();
    assert_eq!(
        out,
        "Name: x\n# some free text\n# PATCH-FIX-UPSTREAM 106.patch gh#o/r#106 -- Fixes it\nPatch0: 106.patch\n"
    );
}

#[test]
fn an_existing_type_one_comment_is_not_replaced() {
    let spec = "Name: x\n# PATCH-FIX-OPENSUSE old reason\nPatch0: 106.patch\n";
    assert_eq!(
        spec_with_comment(spec, "106.patch", "gh#o/r#106", None),
        None
    );
}

#[test]
fn the_insertion_guard_refuses_a_line_below_the_header_boundary() {
    let lines: Vec<&[u8]> = MAIL.split_inclusive('\n').map(str::as_bytes).collect();
    let head = header_len(&lines);
    let below = MAIL.replace("---\n", "---\nUpstream: x\n");
    assert!(!checked_insert(MAIL, &below, 1, head));
    let above = MAIL.replace(" and code\n", " and code\nUpstream: x\n");
    assert!(checked_insert(MAIL, &above, 1, head));
}

#[test]
fn the_reason_is_the_changes_bullet_before_the_subject() {
    let changes = "-------------------------------------------------------------------\n\
Mon Jul 1 2025 - someone\n\n\
- Use pkgconfig for gnu-efi\n\
- Add 106.patch: Improves ARM32 crt0 by splitting header and code; Use pkgconfig\n  for gnu-efi - fixes RISC-V build\n";
    let text = "Subject: [PATCH 1/4] Something else\n\n---\n";
    assert_eq!(
        reason_of(changes, "106.patch", text).as_deref(),
        Some(
            "Improves ARM32 crt0 by splitting header and code; Use pkgconfig for gnu-efi - fixes RISC-V build"
        )
    );
}

#[test]
fn the_reason_falls_back_to_the_subject_without_its_tags() {
    let text = "Subject: [PATCH 1/4] ARM32 crt0: Split\n\n---\n";
    assert_eq!(
        reason_of("", "106.patch", text).as_deref(),
        Some("ARM32 crt0: Split")
    );
    assert_eq!(reason_of("", "106.patch", "--- a/x\n"), None);
}
