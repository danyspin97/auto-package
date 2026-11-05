use serde_json::json;

use super::status_tests::FakeForge;
use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const OLD: &str = "1111111111111111111111111111111111111111";
const OLD2: &str = "2222222222222222222222222222222222222222";
const REPO: &str = "https://github.com/fwupd/fwupd-efi";
const SPEC: &str = "Name: fwupd-efi\nVersion: 1.4\nURL: https://github.com/fwupd/fwupd-efi\n# PATCH-FIX-UPSTREAM gh#fwupd/fwupd-efi#106 -- x\nPatch0: 106.patch\n";
const SPEC_BARE: &str =
    "Name: fwupd-efi\nVersion: 1.4\nURL: https://github.com/fwupd/fwupd-efi\nPatch0: 106.patch\n";

fn mail(sha: &str, subject: &str, body: &str) -> String {
    format!(
        "From {sha} Mon Sep 17 00:00:00 2001\nFrom: A <a@example.org>\nSubject: {subject}\n\nmsg\n---\n{body}"
    )
}

const CODE_A: &str = "--- a/x.c\n+++ b/x.c\n@@ -1 +1 @@\n-old\n+new\n";
const CODE_B: &str = "--- a/x.c\n+++ b/x.c\n@@ -1 +1 @@\n-old\n+newer\n";

fn fake_with(state: serde_json::Value, patch: Option<(&str, String)>) -> FakeForge {
    let mut fake = FakeForge {
        tags: vec!["v1.4".into(), "v1.3".into(), "v1.2".into()],
        ..Default::default()
    };
    fake.json.insert(
        "https://api.github.com/repos/fwupd/fwupd-efi/pulls/106".into(),
        Ok(state),
    );
    if let Some((url, text)) = patch {
        fake.patches.insert(url.to_string(), text);
    }
    fake
}

fn run(fake: &FakeForge, patch: &str, changes: &str) -> Provenance {
    run_spec(fake, patch, changes, SPEC)
}

fn run_spec(fake: &FakeForge, patch: &str, changes: &str, spec: &str) -> Provenance {
    let patches = vec![("106.patch".to_string(), patch.to_string())];
    let inputs = Inputs {
        pkg: "fwupd-efi",
        spec,
        changes,
        version: "1.4",
        repo: Some(REPO.to_string()),
        patches: &patches,
    };
    let mut all = assess(&inputs, fake, true);
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn open_state(head: &str) -> serde_json::Value {
    json!({"state": "open", "merged": false, "head": {"sha": head}, "title": "t"})
}

#[test]
fn an_unchanged_open_request_gets_its_header_and_no_action() {
    let patch = mail(SHA, "[PATCH 1/1] Split", CODE_A);
    let fake = fake_with(open_state(SHA), None);
    let p = run(&fake, &patch, "");
    assert_eq!(p.verdict, Verdict::Nothing);
    let h = p.header.clone().unwrap();
    assert_eq!(h.status, Some("submitted"));
    assert!(fake.calls.borrow().iter().all(|c| !c.ends_with(".patch")));
}

#[test]
fn a_header_is_not_written_for_a_closed_or_unreachable_request() {
    let patch = mail(SHA, "[PATCH 1/1] x", CODE_A);
    let closed = fake_with(
        json!({"state": "closed", "merged": false, "head": {"sha": SHA}}),
        None,
    );
    let p = run(&closed, &patch, "");
    assert_eq!(p.verdict, Verdict::Closed);
    assert_eq!(p.header, None);
    let none = FakeForge::default();
    let p = run(&none, &patch, "");
    assert!(
        matches!(p.verdict, Verdict::NotChecked(_)),
        "{:?}",
        p.verdict
    );
    assert_eq!(p.header, None);
}

#[test]
fn an_open_request_whose_head_moved_with_new_code_is_a_head_move() {
    let patch = mail(SHA, "[PATCH 1/1] Split", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let fake = fake_with(
        open_state(SHA2),
        Some((url, mail(SHA2, "[PATCH 1/1] Split", CODE_B))),
    );
    let p = run(&fake, &patch, "");
    assert_eq!(
        p.verdict,
        Verdict::HeadMoved {
            head: SHA2.into(),
            patch_url: url.into()
        }
    );
}

#[test]
fn a_rebased_head_with_the_same_code_is_nothing() {
    let patch = mail(OLD, "[PATCH 1/1] Split", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let fake = fake_with(
        open_state(SHA2),
        Some((url, mail(SHA2, "[PATCH 1/1] Split", CODE_A))),
    );
    assert_eq!(run(&fake, &patch, "").verdict, Verdict::Nothing);
}

#[test]
fn a_part_of_the_series_is_not_replaced() {
    let patch = mail(OLD, "[PATCH 2/4] Split", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let theirs = format!(
        "{}{}",
        mail(SHA2, "[PATCH 1/4] a", CODE_B),
        mail(SHA2, "[PATCH 2/4] Split", CODE_B)
    );
    let fake = fake_with(open_state(SHA2), Some((url, theirs)));
    assert_eq!(
        run(&fake, &patch, "").verdict,
        Verdict::MovedNotComparable("the package carries part of the request".into())
    );
}

#[test]
fn a_file_without_shas_is_compared_by_code() {
    let patch = format!("Subject: x\n\n---\n{CODE_A}");
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let fake = fake_with(
        open_state(SHA2),
        Some((url, format!("Subject: x\n\n---\n{CODE_B}"))),
    );
    assert_eq!(
        run(&fake, &patch, "").verdict,
        Verdict::MovedNotComparable("the file is not the request's format-patch".into())
    );
}

#[test]
fn a_merged_request_in_the_packaged_release_is_redundant() {
    let patch = mail(OLD, "[PATCH 1/1] Split", CODE_A);
    let merged = json!({"state": "closed", "merged": true, "merge_commit_sha": SHA2, "head": {"sha": SHA}, "title": "t"});
    let mut fake = fake_with(merged, None);
    fake.json.insert(
        format!("https://api.github.com/repos/fwupd/fwupd-efi/compare/v1.4...{SHA2}"),
        Ok(json!({"status": "identical"})),
    );
    let p = run(&fake, &patch, "");
    assert_eq!(p.verdict, Verdict::Redundant { tag: "v1.4".into() });
    assert_eq!(p.header.unwrap().status, Some("merged"));
}

#[test]
fn a_merged_request_in_a_newer_release_is_released_later() {
    let patch = mail(OLD, "[PATCH 1/1] Split", CODE_A);
    let merged = json!({"state": "closed", "merged": true, "merge_commit_sha": SHA2, "head": {"sha": SHA}, "title": "t"});
    let mut fake = fake_with(merged, None);
    fake.tags = vec!["v1.5".into(), "v1.4".into()];
    fake.json.insert(
        format!("https://api.github.com/repos/fwupd/fwupd-efi/compare/v1.4...{SHA2}"),
        Ok(json!({"status": "diverged"})),
    );
    fake.json.insert(
        format!("https://api.github.com/repos/fwupd/fwupd-efi/compare/v1.5...{SHA2}"),
        Ok(json!({"status": "behind"})),
    );
    assert_eq!(
        run(&fake, &patch, "").verdict,
        Verdict::ReleasedLater {
            tag: "v1.5".into(),
            version: "1.5".into()
        }
    );
}

#[test]
fn a_file_name_alone_needs_a_matching_request_patch() {
    let patch = mail(SHA, "[PATCH 1/1] Split", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let other = mail(SHA2, "[PATCH 1/1] Other thing", CODE_B);
    let fake = fake_with(open_state(SHA), Some((url, other)));
    let p = run_spec(&fake, &patch, "", SPEC_BARE);
    assert_eq!(p.origin, None);
    assert!(
        matches!(p.verdict, Verdict::NotChecked(_)),
        "{:?}",
        p.verdict
    );
    let confirmed = mail(SHA, "[PATCH 1/1] Split", CODE_B);
    let fake = fake_with(open_state(SHA), Some((url, confirmed)));
    let p = run_spec(&fake, &patch, "", SPEC_BARE);
    assert_eq!(p.origin.map(|o| o.evidence), Some(OriginEvidence::FileName));
}

#[test]
fn a_patch_with_an_upstream_line_says_so_and_no_origin_is_a_nothing() {
    let patch = format!(
        "Upstream: merged https://github.com/fwupd/fwupd-efi/pull/106\n{}",
        mail(SHA, "[PATCH] x", CODE_A)
    );
    let fake = fake_with(open_state(SHA), None);
    let p = run(&fake, &patch, "");
    assert!(p.has_upstream_line);
    let plain = run_spec(
        &FakeForge::default(),
        "Subject: x\n\n---\n--- a/x.c\n+++ b/x.c\n@@ -1 +1 @@\n-a\n+b\n",
        "",
        "Name: x\nVersion: 1\n",
    );
    assert_eq!(plain.verdict, Verdict::Nothing);
    assert_eq!(plain.origin, None);
}

#[test]
fn the_header_and_the_spec_comment_follow_the_plan() {
    let patch = mail(SHA, "[PATCH 1/1] Split", CODE_A);
    let fake = fake_with(open_state(SHA), None);
    let p = run(&fake, &patch, "");
    let out = header_for(&p, &patch).unwrap();
    assert!(out.contains("Upstream: submitted https://github.com/fwupd/fwupd-efi/pull/106\n"));
    assert!(out.contains("References: gh#fwupd/fwupd-efi#106\n"));
    // The spec already has a type 1 comment above the patch line, so none is added.
    assert_eq!(spec_comment_for(&p, SPEC), None);
}

#[test]
fn an_unmoved_part_of_the_request_is_nothing_not_moved() {
    // The file is `[PATCH 2/4]` of a series whose head did not move: its lines
    // all stand in the request, so nothing is re-downloaded or replaced.
    let patch = mail(OLD, "[PATCH 2/4] Split", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let theirs = format!(
        "{}{}",
        mail(OLD2, "[PATCH 1/4] a", CODE_B),
        mail(SHA, "[PATCH 2/4] Split", CODE_A)
    );
    let fake = fake_with(open_state(SHA), Some((url, theirs)));
    assert_eq!(run(&fake, &patch, "").verdict, Verdict::Nothing);
}

#[test]
fn a_generic_subject_alone_does_not_confirm_a_file_name() {
    let patch = mail(SHA, "[PATCH 1/1] Fix build", CODE_A);
    let url = "https://github.com/fwupd/fwupd-efi/pull/106.patch";
    let other = "Subject: [PATCH 1/1] Fix build\n\n---\n--- a/other.c\n+++ b/other.c\n@@ -1 +1 @@\n-p\n+q\n";
    let fake = fake_with(open_state(SHA2), Some((url, other.to_string())));
    let p = run_spec(&fake, &patch, "", SPEC_BARE);
    assert_eq!(p.origin, None);
}

#[test]
fn a_merged_request_whose_containment_is_not_checked_writes_no_header() {
    let patch = mail(OLD, "[PATCH 1/1] Split", CODE_A);
    let merged = json!({"state": "closed", "merged": true, "merge_commit_sha": SHA2, "head": {"sha": SHA}, "title": "t"});
    // The tags are listed but no compare answers: the containment is not checked.
    let fake = fake_with(merged, None);
    let p = run(&fake, &patch, "");
    assert!(
        matches!(p.containment, Some(Containment::NotChecked(_))),
        "{:?}",
        p.containment
    );
    assert_eq!(p.header, None);
}
