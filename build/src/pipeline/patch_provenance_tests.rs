//! The provenance stage of a run: drops with their proof, headers written, notes.

use super::patch_provenance::{
    provenance_stage_with, try_upstream_revisions, upstream_revisions_wanted,
};
use super::patch_review::ReviewTurn;
use super::patch_stage::PatchCheckFn;
use super::patch_stage::write_refresh_bullets;
use super::*;
use crate::patch_provenance::status_tests::FakeForge;
use crate::pipeline::patch_stage_tests::{UPSTREAM_DIFF, quilt_missing, source_tarball};
use serde_json::json;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const OLD_SHA: &str = "1111111111111111111111111111111111111111";
const OLD2: &str = "2222222222222222222222222222222222222222";
const SPEC_HEAD: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\nURL: https://github.com/o/r\n";

struct Case {
    accepted: Vec<String>,
    reviewer: Vec<String>,
    replaced: Vec<String>,
    _root: tempfile::TempDir,
    checkout: PathBuf,
    spec: String,
    changes: String,
    notes: String,
    warnings: Vec<String>,
}

/// A checkout with `patches` (name, body) declared in order after `comment`,
/// and the stage run over it with the given quilt check and review turn.
fn stage_case(
    patches: &[(&str, &str)],
    comment: &str,
    net: &FakeForge,
    check: &mut PatchCheckFn<'_>,
    review: &mut ReviewTurn<'_>,
    update: bool,
) -> Case {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.patch_provenance = true;
    config.no_agent = false;
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
    let mut st = state();
    let mut accepted: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
    if update {
        let failing: Vec<String> = patches.iter().map(|(n, _)| n.to_string()).collect();
        let mut refreshed = std::collections::BTreeSet::new();
        let mut lines = Vec::new();
        let _ = try_upstream_revisions(
            &ctx,
            &mut st,
            ap_core::event::StepId(5),
            check,
            review,
            net,
            &failing,
            &mut accepted,
            &mut refreshed,
            &mut lines,
        );
        write_refresh_bullets(&ctx, ap_core::event::StepId(5), &refreshed);
    } else {
        provenance_stage_with(&ctx, net);
    }
    Case {
        replaced: crate::pipeline::review_marks::replaced_patches(&ctx.ws, &checkout),
        accepted: accepted.keys().cloned().collect(),
        reviewer: crate::pipeline::review_marks::reviewer_rewrites(&ctx.ws, &checkout),
        _root: root,
        checkout: checkout.clone(),
        spec: std::fs::read_to_string(&spec_path).unwrap(),
        changes: std::fs::read_to_string(checkout.join("foo.changes")).unwrap(),
        notes: std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default(),
        warnings: log.0.lock().unwrap().clone(),
    }
}

/// [`stage_case`] where no quilt check or review turn may run (header and drop
/// tests): a check asked is a failure.
fn run(patches: &[(&str, &str)], comment: &str, net: &FakeForge) -> Case {
    let mut check = |_: &RunCtx, _: crate::patch_check::Refresh| crate::patch_check::CheckReport {
        patches: Vec::new(),
        not_run: None,
    };
    let mut review = |_: &RunCtx,
                      _: &mut RunState,
                      _: &str,
                      _: &str|
     -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
        panic!("no review turn expected")
    };
    stage_case(patches, comment, net, &mut check, &mut review, false)
}

/// A run state for a stage test (the fields the stage reads are default).
fn state() -> RunState {
    RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        run_start_spec: None,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
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

mod update;
