//! The replacement and update-loop tests of the provenance stage.

use super::*;

/// Runs the replacement path: the quilt check applies everything, and the one
/// review turn answers `verdict`.
fn run_replacing(patches: &[(&str, &str)], net: &FakeForge, accept: bool, update: bool) -> Case {
    run_replacing_with(patches, COMMENT, net, accept, update)
}

fn run_replacing_with(
    patches: &[(&str, &str)],
    comment: &str,
    net: &FakeForge,
    accept: bool,
    update: bool,
) -> Case {
    let mut check = |ctx: &RunCtx, _: crate::patch_check::Refresh| {
        let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
        crate::patch_check::CheckReport {
            patches: crate::patch_check::declared_patches(&spec)
                .into_iter()
                .map(|(_, name)| crate::patch_check::PatchResult {
                    name,
                    level: 1,
                    status: crate::patch_check::PatchStatus::Applies,
                    refreshed: None,
                })
                .collect(),
            not_run: None,
        }
    };
    let mut review = |ctx: &RunCtx,
                      _: &mut RunState,
                      _: &str,
                      _: &str|
     -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
        let dir = ctx.ws.dir().join("review-step");
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = pipeline_wheel_outcome("x");
        out.patch_review = Some(crate::agent::PatchReviewAnswer {
            verdict: if accept {
                crate::agent::PatchReviewVerdict::Accept
            } else {
                crate::agent::PatchReviewVerdict::Reject
            },
            reason: "checked".into(),
            problems: Vec::new(),
        });
        Ok((
            StepDir {
                path: dir,
                id: ap_core::event::StepId(9),
            },
            out,
        ))
    };
    stage_case(patches, comment, net, &mut check, &mut review, update)
}

#[test]
fn a_moved_request_revision_that_the_review_accepts_replaces_the_patch() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let fresh = mail(SHA, UPSTREAM_DIFF);
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    net.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        fresh.clone(),
    );
    let c = run_replacing(&[("106.patch", &carried)], &net, true, true);
    let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
    assert!(
        patch.contains("+B\n"),
        "{patch}\nNOTES {}\nWARN {:?}",
        c.notes,
        c.warnings
    );
    assert!(
        patch.contains("Upstream: submitted https://github.com/o/r/pull/106\n"),
        "{patch}"
    );
    assert!(c.changes.contains("Refresh 106.patch"), "{}", c.changes);
}

#[test]
fn a_moved_request_revision_the_review_rejects_is_not_taken() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    net.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        mail(SHA, UPSTREAM_DIFF),
    );
    let c = run_replacing(&[("106.patch", &carried)], &net, false, true);
    let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
    assert_eq!(patch, carried);
    assert!(!c.changes.contains("Refresh 106.patch"), "{}", c.changes);
}

#[test]
fn the_update_loop_takes_a_moved_revision_and_marks_it_accepted() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    net.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        mail(SHA, UPSTREAM_DIFF),
    );
    let c = run_replacing(&[("106.patch", &carried)], &net, true, true);
    assert_eq!(c.accepted, ["106.patch".to_string()]);
    assert_eq!(c.replaced, ["106.patch".to_string()]);
    let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
    assert!(patch.contains("+B\n"), "{patch}");
}

#[test]
fn two_replaced_patches_share_one_grouped_refresh_bullet() {
    let carried = |n: u32| {
        format!(
            "From {OLD_SHA} Mon Sep 17 00:00:00 2001\nFrom: A <a@example.org>\nSubject: [PATCH 1/1] Fix f\nUpstream: submitted https://github.com/o/r/pull/{n}\n\nmsg\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n"
        )
    };
    let (c106, c107) = (carried(106), carried(107));
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    for n in [106u32, 107] {
        net.json.insert(
            format!("https://api.github.com/repos/o/r/pulls/{n}"),
            Ok(json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"})),
        );
        net.patches.insert(
            format!("https://github.com/o/r/pull/{n}.patch"),
            mail(SHA, UPSTREAM_DIFF),
        );
    }
    let c = run_replacing_with(
        &[("106.patch", &c106), ("107.patch", &c107)],
        "",
        &net,
        true,
        true,
    );
    assert_eq!(c.accepted.len(), 2, "{:?}", c.accepted);
    let refresh: Vec<&str> = c
        .changes
        .lines()
        .filter(|l| l.contains("Refresh"))
        .collect();
    assert_eq!(refresh, ["- Refresh patches:"], "{}", c.changes);
    assert!(c.changes.contains("  * 106.patch"), "{}", c.changes);
    assert!(c.changes.contains("  * 107.patch"), "{}", c.changes);
}

/// A moved request whose revision is `diff`, carried as a valid patch.
fn moved_case(diff: &str, carried: &str) -> Case {
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    net.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        mail(SHA, diff),
    );
    run_replacing(&[("106.patch", carried)], &net, true, true)
}

#[test]
fn a_revision_that_breaks_a_patch_rule_is_not_taken() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let cases = [
        // A path that leaves the tree.
        "--- a/f.txt\n+++ b/../../evil.txt\n@@ -1 +1 @@\n-x\n+X\n",
        // A deleted test file.
        "--- a/tests/test_x.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-assert True\n",
        // A failure hidden by `|| true`.
        "--- a/build.sh\n+++ b/build.sh\n@@ -1 +1 @@\n-make check\n+make check || true\n",
        // -Wno-error without a comment.
        "--- a/Makefile\n+++ b/Makefile\n@@ -1 +1 @@\n-CFLAGS += -O2\n+CFLAGS += -O2 -Wno-error\n",
    ];
    for diff in cases {
        let c = moved_case(diff, &carried);
        let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
        assert_eq!(patch, carried, "{diff}");
        assert!(c.accepted.is_empty(), "{diff}");
    }
}

#[test]
fn a_revision_keeps_the_maintainers_header_lines() {
    let carried = format!(
        "From {OLD_SHA} Mon Sep 17 00:00:00 2001\nFrom: A <a@example.org>\nSubject: [PATCH 1/1] Fix f\nDescription: kept by the maintainer\n\nmsg\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n"
    );
    let c = moved_case(UPSTREAM_DIFF, &carried);
    let patch = std::fs::read_to_string(c.checkout.join("106.patch")).unwrap();
    assert!(patch.contains("+B\n"), "{patch}");
    assert_eq!(
        patch.matches("Description: kept by the maintainer").count(),
        1,
        "{patch}"
    );
    assert!(
        patch.contains("Upstream: submitted https://github.com/o/r/pull/106\n"),
        "{patch}"
    );
}

#[test]
fn a_reviewer_rewrite_inside_a_replacement_is_recorded() {
    let carried = mail(OLD_SHA, "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+X\n");
    let mut net = FakeForge {
        tags: vec!["v1.0".into()],
        ..Default::default()
    };
    open_pr(
        &mut net,
        json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"}),
    );
    net.patches.insert(
        "https://github.com/o/r/pull/106.patch".into(),
        mail(SHA, UPSTREAM_DIFF),
    );
    let mut check = |ctx: &RunCtx, _: crate::patch_check::Refresh| {
        let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
        crate::patch_check::CheckReport {
            patches: crate::patch_check::declared_patches(&spec)
                .into_iter()
                .map(|(_, name)| crate::patch_check::PatchResult {
                    name,
                    level: 1,
                    status: crate::patch_check::PatchStatus::Applies,
                    refreshed: None,
                })
                .collect(),
            not_run: None,
        }
    };
    // The first answer rewrites the revision and says so; the next accepts it.
    let turns = std::cell::Cell::new(0u32);
    let mut review = |ctx: &RunCtx,
                      _: &mut RunState,
                      _: &str,
                      _: &str|
     -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
        let dir = ctx.ws.dir().join("review-step");
        std::fs::create_dir_all(&dir).unwrap();
        let first = turns.get() == 0;
        turns.set(turns.get() + 1);
        if first {
            std::fs::write(
                ctx.checkout_dir.join("106.patch"),
                format!(
                    "From {SHA} Mon Sep 17 00:00:00 2001\nFrom: A <a@example.org>\nSubject: [PATCH 1/1] Fix f\nUpstream: submitted https://github.com/o/r/pull/106\n\nmsg\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+C\n"
                ),
            )
            .unwrap();
        }
        let mut out = pipeline_wheel_outcome("x");
        out.patch_review = Some(crate::agent::PatchReviewAnswer {
            verdict: if first {
                crate::agent::PatchReviewVerdict::Rewrite
            } else {
                crate::agent::PatchReviewVerdict::Accept
            },
            reason: "checked".into(),
            problems: Vec::new(),
        });
        Ok((
            StepDir {
                path: dir,
                id: ap_core::event::StepId(9),
            },
            out,
        ))
    };
    let c = stage_case(
        &[("106.patch", &carried)],
        "",
        &net,
        &mut check,
        &mut review,
        true,
    );
    assert_eq!(c.reviewer, ["106.patch".to_string()]);
    assert_eq!(c.accepted, ["106.patch".to_string()]);
}

#[test]
fn no_revision_is_asked_under_no_agent_or_without_failing_patches() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = super::hygiene_tests::config("foo", &tmp.path().join("state"));
    config.patch_provenance = true;
    config.update = true;
    let failing = ["106.patch".to_string()];
    config.no_agent = true;
    assert!(!upstream_revisions_wanted(&config, &failing));
    config.no_agent = false;
    assert!(upstream_revisions_wanted(&config, &failing));
    config.update = false;
    assert!(!upstream_revisions_wanted(&config, &failing));
    config.update = true;
    assert!(!upstream_revisions_wanted(&config, &[]));
    config.patch_provenance = false;
    assert!(!upstream_revisions_wanted(&config, &failing));
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
