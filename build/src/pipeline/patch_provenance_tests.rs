//! The provenance stage of a run: drops with their proof, headers written, notes.

use super::patch_provenance::provenance_stage_with;
use super::*;
use crate::patch_provenance::status_tests::FakeForge;
use crate::pipeline::patch_stage_tests::{UPSTREAM_DIFF, quilt_missing, source_tarball};
use serde_json::json;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const OLD_SHA: &str = "1111111111111111111111111111111111111111";
const OLD2: &str = "2222222222222222222222222222222222222222";
const SPEC_HEAD: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\nURL: https://github.com/o/r\n";

struct Case {
    _root: tempfile::TempDir,
    checkout: PathBuf,
    spec: String,
    changes: String,
    notes: String,
    warnings: Vec<String>,
}

/// A checkout with `patches` (name, body) declared in order after `comment`.
fn run(patches: &[(&str, &str)], comment: &str, net: &FakeForge) -> Case {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.patch_provenance = true;
    config.budget_usd = 100.0;
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    source_tarball(&checkout);
    let mut spec = SPEC_HEAD.to_string();
    for (i, (name, body)) in patches.iter().enumerate() {
        spec.push_str(comment);
        spec.push_str(&format!("Patch{i}: {name}\n"));
        std::fs::write(checkout.join(name), body).unwrap();
    }
    spec.push_str("\n%prep\n%autosetup -p1\n");
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, &spec).unwrap();
    let sep = "-".repeat(67);
    std::fs::write(
        checkout.join("foo.changes"),
        format!("{sep}\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n"),
    )
    .unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let log = ReviewLog::default();
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &log,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
        signals: Default::default(),
    };
    provenance_stage_with(&ctx, net);
    Case {
        _root: root,
        checkout: checkout.clone(),
        spec: std::fs::read_to_string(&spec_path).unwrap(),
        changes: std::fs::read_to_string(checkout.join("foo.changes")).unwrap(),
        notes: std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default(),
        warnings: log.0.lock().unwrap().clone(),
    }
}

/// The warnings the stage reports.
#[derive(Default)]
struct ReviewLog(std::sync::Mutex<Vec<String>>);

impl Reporter for ReviewLog {
    fn report(&self, e: &Event) {
        if let Event::Warning { message, .. } = e {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

fn mail(sha: &str, body: &str) -> String {
    format!(
        "From {sha} Mon Sep 17 00:00:00 2001\nFrom: A <a@example.org>\nSubject: [PATCH 1/1] Fix f\n\nmsg\n---\n{body}"
    )
}

fn open_pr(net: &mut FakeForge, state: serde_json::Value) {
    net.json.insert(
        "https://api.github.com/repos/o/r/pulls/106".into(),
        Ok(state),
    );
}

const COMMENT: &str = "# https://github.com/o/r/pull/106\n";

#[test]
fn an_open_request_gets_its_header_and_spec_comment_and_no_bullet() {
    let body = mail(SHA, UPSTREAM_DIFF);
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    let c = run(&[("106.patch", &body)], COMMENT, &net);
    let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
    assert!(
        patch.contains("Upstream: submitted https://github.com/o/r/pull/106\n"),
        "{patch}"
    );
    assert!(
        c.spec.contains("# PATCH-FIX-UPSTREAM 106.patch gh#o/r#106"),
        "{}",
        c.spec
    );
    assert_eq!(
        c.changes.lines().filter(|l| l.starts_with("- ")).count(),
        1,
        "{}",
        c.changes
    );
    assert!(c.notes.contains("## Patch provenance"), "{}", c.notes);
}

#[test]
fn a_merged_request_in_the_packaged_release_is_dropped_with_its_proof() {
    assert!(!quilt_missing(), "the removal proof needs quilt");
    // The packaged source has `B`; the patch turns `b` into `B`, so it reverse-applies.
    let body = mail(SHA, UPSTREAM_DIFF);
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "closed", "merged": true, "merge_commit_sha": SHA, "head": {"sha": SHA}, "title": "t"}),
    );
    net.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.0...{SHA}"),
        Ok(json!({"status": "identical"})),
    );
    let c = run(&[("106.patch", &body)], COMMENT, &net);
    assert!(!c.spec.contains("106.patch"), "{}", c.spec);
    assert!(!c.checkout.join("106.patch").exists());
    assert!(
        c.changes
            .contains("Drop 106.patch: already applied upstream"),
        "{}",
        c.changes
    );
}

#[test]
fn a_merged_request_whose_proof_fails_keeps_the_patch_with_a_warning() {
    assert!(!quilt_missing(), "the removal proof needs quilt");
    let body = mail(SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-zz\n+yy\n");
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "closed", "merged": true, "merge_commit_sha": SHA, "head": {"sha": SHA}, "title": "t"}),
    );
    net.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.0...{SHA}"),
        Ok(json!({"status": "identical"})),
    );
    let c = run(&[("106.patch", &body)], COMMENT, &net);
    assert!(c.spec.contains("Patch0: 106.patch"), "{}", c.spec);
    assert!(c.checkout.join("106.patch").exists());
    assert!(
        c.warnings.iter().any(|w| w.contains("removal proof fails")),
        "{:?}",
        c.warnings
    );
}

#[test]
fn a_closed_a_moved_and_a_later_released_request_are_noted_and_kept() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let mut closed = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut closed,
        json!({"state": "closed", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    let c = run(&[("106.patch", &carried)], COMMENT, &closed);
    assert!(c.notes.contains("closed without a merge"), "{}", c.notes);
    assert!(c.spec.contains("Patch0: 106.patch"));

    let mut moved = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut moved,
        json!({"state": "open", "merged": false, "head": {"sha": OLD2}, "title": "t"}),
    );
    moved.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        mail(OLD2, UPSTREAM_DIFF),
    );
    let c = run(&[("106.patch", &carried)], COMMENT, &moved);
    assert!(c.notes.contains("moved with different code"), "{}", c.notes);
    assert!(
        !std::fs::read_to_string(c.checkout.join("106.patch"))
            .unwrap()
            .contains("+B")
    );

    let mut later = FakeForge {
        tags: vec!["v1.1".into(), "v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut later,
        json!({"state": "closed", "merged": true, "merge_commit_sha": OLD2, "head": {"sha": SHA}, "title": "t"}),
    );
    later.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.0...{OLD2}"),
        Ok(json!({"status": "diverged"})),
    );
    later.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.1...{OLD2}"),
        Ok(json!({"status": "behind"})),
    );
    let c = run(&[("106.patch", &carried)], COMMENT, &later);
    assert!(c.notes.contains("merged upstream in v1.1"), "{}", c.notes);
    assert!(c.spec.contains("Patch0: 106.patch"));
}
