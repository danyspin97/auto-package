//! Tests of the review rounds of one patch: the pure parts, and
//! `review_patch_rounds` with a fake apply check and a scripted turn.

use super::patch_review::*;
use super::patch_review_prompt::*;
use super::*;
use crate::agent::{Action, PatchReviewAnswer, PatchReviewVerdict as V};
use std::sync::Mutex;

fn outcome(action: Action, verdict: Option<(V, &str, &[&str])>) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.reasoning = "scripted".into();
    out.patch_review = verdict.map(|(verdict, reason, problems)| PatchReviewAnswer {
        verdict,
        reason: reason.into(),
        problems: problems.iter().map(|p| p.to_string()).collect(),
    });
    out
}

#[test]
fn a_review_answer_and_the_bytes_make_one_move() {
    let said = |v, reason: &str| outcome(Action::NoChange, Some((v, reason, &["p1", "p2"])));
    assert_eq!(
        review_move(&said(V::Accept, "ok"), false),
        ReviewMove::Accept
    );
    assert_eq!(
        review_move(&said(V::Accept, "ok"), true),
        ReviewMove::Rewrite
    );
    assert_eq!(
        review_move(&said(V::Rewrite, "ok"), true),
        ReviewMove::Rewrite
    );
    assert!(matches!(
        review_move(&said(V::Rewrite, "ok"), false),
        ReviewMove::Slip(m) if m.contains("unchanged")
    ));
    for changed in [false, true] {
        assert_eq!(
            review_move(&said(V::Reject, "bad"), changed),
            ReviewMove::Reject("bad; p1; p2".into())
        );
    }
    let blocked = outcome(
        Action::Blocked {
            why: "cannot judge".into(),
        },
        None,
    );
    assert_eq!(
        review_move(&blocked, true),
        ReviewMove::GiveUp("cannot judge".into())
    );
    assert!(matches!(
        review_move(&outcome(Action::NoChange, None), false),
        ReviewMove::Slip(_)
    ));
}

static TARGETS: std::sync::LazyLock<Vec<String>> =
    std::sync::LazyLock::new(|| vec!["src/x.c".to_string(), "src/y.c".to_string()]);

fn input<'a>(round: usize, original: Option<&'a str>, current: &'a str) -> ReviewInput<'a> {
    ReviewInput {
        round,
        name: "a.patch",
        level: 1,
        spec_comment: Some("# PATCH-FIX-UPSTREAM a.patch"),
        cause: "rebased onto 2.0 by a refresh turn",
        targets: &TARGETS,
        quilt: "the patch applies",
        original,
        current,
        rollback: None,
    }
}

#[test]
fn the_context_names_the_round_and_the_last_round_only_in_the_last() {
    let first = patch_review_context(&input(1, Some("old"), "new"));
    assert!(first.starts_with("Review round 1 of at most 3."), "{first}");
    assert!(!first.contains("last round"), "{first}");
    let last = patch_review_context(&input(3, Some("old"), "new"));
    assert!(last.contains("This is the last round"), "{last}");
    assert!(first.contains("src/x.c, src/y.c"), "{first}");
    assert!(
        first.contains("<untrusted-data source=\"patch file names\">"),
        "{first}"
    );
    for label in ["spec comment", "quilt output"] {
        assert!(
            first.contains(&format!("source=\"{label}\"")),
            "{label}: {first}"
        );
    }
    assert!(first.contains("# PATCH-FIX-UPSTREAM a.patch"), "{first}");
    assert!(first.contains("patch-review/a.patch.orig"), "{first}");
    let mut rolled = input(2, Some("old"), "new");
    rolled.rollback = Some("it does not apply");
    assert!(patch_review_context(&rolled).contains(
        "Your last rewrite was rolled back:\n<untrusted-data source=\"rollback reason\">"
    ));
}

#[test]
fn the_context_wraps_the_texts_and_says_when_the_patch_is_new() {
    let new = patch_review_context(&input(1, None, "+x"));
    assert!(new.contains("none: the patch is new"), "{new}");
    for label in ["patch now", "patch file diff"] {
        assert!(
            new.contains(&format!("<untrusted-data source=\"{label}\">")),
            "{new}"
        );
    }
    assert!(!new.contains("source=\"patch before\""), "{new}");
    let old = patch_review_context(&input(1, Some("-y"), "+x"));
    assert!(
        old.contains("<untrusted-data source=\"patch before\">"),
        "{old}"
    );
}

#[test]
fn a_huge_patch_is_cut_in_the_context() {
    let big = "+x\n".repeat(100_000);
    let text = patch_review_context(&input(1, Some(&big), &big));
    assert!(text.len() < 120_000, "{}", text.len());
    assert!(text.contains("patch-review/a.patch.orig"));
}

#[test]
fn the_spec_comment_is_the_patch_line_above_the_patch() {
    let spec = "Name: foo\nVersion: 1\n# PATCH-FIX-OPENSUSE fix-it.patch boo#1\nPatch0: %{name}-fix.patch\n";
    assert_eq!(
        spec_patch_comment(spec, "foo-fix.patch").as_deref(),
        Some("# PATCH-FIX-OPENSUSE fix-it.patch boo#1")
    );
    assert_eq!(spec_patch_comment(spec, "other.patch"), None);
}

// ── review_patch_rounds ──────────────────────────────────────────────────

const OLD: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/src/foo.c\n+++ b/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 0;\n";
const NEW: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/src/foo.c\n+++ b/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 1;\n";
const BY_REVIEW: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/src/foo.c\n+++ b/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 2;\n";

#[derive(Default)]
struct Collect(Mutex<Vec<(String, String, String)>>);

impl Reporter for Collect {
    fn report(&self, e: &Event) {
        if let Event::PatchReviewed {
            patch,
            verdict,
            reason,
            ..
        } = e
        {
            self.0
                .lock()
                .unwrap()
                .push((patch.clone(), verdict.clone(), reason.clone()));
        }
    }
}

struct Run {
    end: Result<PatchReviewEnd, PipelineResult>,
    verdicts: Vec<String>,
    contexts: Vec<String>,
    patch: String,
    spec: String,
    other: String,
    lines: Vec<String>,
}

/// Reviews `a.patch` (at `NEW`, the patch before being `OLD`) with `script` as
/// the review turns; `apply` says whether a patch applies.
fn review(
    spent: f64,
    apply: &mut dyn FnMut(&str) -> Result<(), String>,
    script: &mut dyn FnMut(
        usize,
        &Path,
    ) -> Result<crate::agent::TurnOutcome, (String, FailureKind)>,
) -> Run {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.budget_usd = 10.0;
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_text = "Name: foo\nVersion: 1.0\n# PATCH-FIX-UPSTREAM a.patch\nPatch0: a.patch\nPatch1: b.patch\n\n%prep\n%autosetup -p1\n";
    std::fs::write(checkout.join("foo.spec"), spec_text).unwrap();
    std::fs::write(checkout.join("a.patch"), NEW).unwrap();
    std::fs::write(checkout.join("b.patch"), OLD).unwrap();
    let run_dir = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run_dir.pkg("foo");
    let reporter = Collect::default();
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &reporter,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: checkout.join("foo.spec"),
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 1,
        spent_usd: spent,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
    };
    let step_path = root.path().join("step");
    std::fs::create_dir_all(&step_path).unwrap();
    let mut contexts = Vec::new();
    let mut round = 0usize;
    let mut lines = Vec::new();
    let end = review_patch_rounds(
        &ctx,
        &mut st,
        ap_core::event::StepId(1),
        "a.patch",
        Some(OLD.as_bytes()),
        "rebased onto 2.0 by a refresh turn",
        &mut |_, name| apply(name),
        &mut |ctx, _, _, context| {
            contexts.push(context.to_string());
            round += 1;
            let out = script(round, &ctx.checkout_dir)?;
            Ok((
                StepDir {
                    path: step_path.clone(),
                    id: ap_core::event::StepId(2),
                },
                out,
            ))
        },
        &mut lines,
    );
    let read = |n: &str| std::fs::read_to_string(checkout.join(n)).unwrap();
    Run {
        end,
        verdicts: reporter
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.1.clone())
            .collect(),
        contexts,
        patch: read("a.patch"),
        spec: read("foo.spec"),
        other: read("b.patch"),
        lines,
    }
}

fn say(verdict: V, reason: &str) -> Result<crate::agent::TurnOutcome, (String, FailureKind)> {
    Ok(outcome(Action::NoChange, Some((verdict, reason, &[]))))
}

#[test]
fn an_accept_ends_the_review_with_one_event_and_one_line() {
    let r = review(0.0, &mut |_| Ok(()), &mut |_, _| {
        say(V::Accept, "same intent")
    });
    assert_eq!(r.end.unwrap(), PatchReviewEnd::Accepted { rewrote: false });
    assert_eq!(r.verdicts, ["accepted"]);
    assert_eq!(r.patch, NEW);
    assert!(
        r.lines[0].contains("review round 1: accepted: same intent"),
        "{:?}",
        r.lines
    );
}

#[test]
fn a_rewrite_that_applies_is_reviewed_by_the_next_round() {
    let r = review(0.0, &mut |_| Ok(()), &mut |round, co| {
        if round == 1 {
            std::fs::write(co.join("a.patch"), BY_REVIEW).unwrap();
            say(V::Rewrite, "fixed the value")
        } else {
            say(V::Accept, "now right")
        }
    });
    assert_eq!(r.end.unwrap(), PatchReviewEnd::Accepted { rewrote: true });
    assert_eq!(r.verdicts, ["rewritten", "accepted"]);
    assert_eq!(r.patch, BY_REVIEW);
    assert!(r.contexts[1].contains("int x = 2;"), "{}", r.contexts[1]);
}

#[test]
fn a_rewrite_that_does_not_apply_is_rolled_back_and_the_next_round_hears_why() {
    let mut calls = 0;
    let r = review(
        0.0,
        &mut |_| {
            calls += 1;
            // The status for the prompt passes; the gate's check (the second) fails.
            if calls == 2 {
                Err("Hunk #1 FAILED".into())
            } else {
                Ok(())
            }
        },
        &mut |round, co| {
            if round == 1 {
                std::fs::write(co.join("a.patch"), BY_REVIEW).unwrap();
                say(V::Rewrite, "try")
            } else {
                say(V::Accept, "the writer's is fine")
            }
        },
    );
    assert_eq!(r.end.unwrap(), PatchReviewEnd::Accepted { rewrote: false });
    assert_eq!(r.verdicts, ["rolled_back", "accepted"]);
    assert_eq!(r.patch, NEW);
    assert!(
        r.contexts[1].contains("the rewrite does not apply: Hunk #1 FAILED"),
        "{}",
        r.contexts[1]
    );
}

#[test]
fn a_reviewer_that_edits_the_spec_and_another_patch_has_both_put_back() {
    let r = review(0.0, &mut |_| Ok(()), &mut |round, co| {
        if round == 1 {
            std::fs::write(co.join("foo.spec"), "Name: evil\n").unwrap();
            std::fs::write(co.join("b.patch"), "evil\n").unwrap();
        }
        say(V::Accept, "fine")
    });
    assert_eq!(r.end.unwrap(), PatchReviewEnd::Accepted { rewrote: false });
    assert!(r.spec.starts_with("Name: foo"), "{}", r.spec);
    assert_eq!(r.other, OLD);
}

#[test]
fn a_stray_edit_is_told_to_the_next_round() {
    let r = review(0.0, &mut |_| Ok(()), &mut |round, co| {
        if round == 1 {
            std::fs::write(co.join("b.patch"), "evil\n").unwrap();
            say(V::Rewrite, "no edit made")
        } else {
            say(V::Accept, "fine")
        }
    });
    assert_eq!(r.end.unwrap(), PatchReviewEnd::Accepted { rewrote: false });
    assert!(
        r.contexts[1].contains("only a.patch may change; your edit to b.patch was rolled back"),
        "{}",
        r.contexts[1]
    );
    assert_eq!(r.other, OLD);
}

#[test]
fn a_reject_and_a_give_up_restore_the_patch_and_end_the_review() {
    let r = review(0.0, &mut |_| Ok(()), &mut |_, co| {
        std::fs::write(co.join("a.patch"), BY_REVIEW).unwrap();
        Ok(outcome(
            Action::NoChange,
            Some((V::Reject, "drops a hunk", &["hunk 2 is gone"])),
        ))
    });
    assert_eq!(
        r.end.unwrap(),
        PatchReviewEnd::Rejected("drops a hunk; hunk 2 is gone".into())
    );
    assert_eq!(r.patch, NEW);
    let r = review(0.0, &mut |_| Ok(()), &mut |_, _| {
        Ok(outcome(
            Action::Blocked {
                why: "cannot follow it".into(),
            },
            None,
        ))
    });
    assert_eq!(
        r.end.unwrap(),
        PatchReviewEnd::GaveUp("cannot follow it".into())
    );
    assert_eq!(r.verdicts, ["gave_up"]);
}

#[test]
fn three_rounds_without_an_accept_leave_the_review_unsettled() {
    let r = review(0.0, &mut |_| Ok(()), &mut |_, _| {
        say(V::Rewrite, "I rewrote it")
    });
    assert!(matches!(r.end.unwrap(), PatchReviewEnd::Unsettled(_)));
    assert_eq!(r.verdicts, ["slip", "slip", "slip"]);
    assert_eq!(r.contexts.len(), 3);
    assert!(r.contexts[2].contains("This is the last round"));
    assert_eq!(r.patch, NEW);
}

#[test]
fn a_rewrite_in_the_last_round_is_rolled_back_and_unsettled() {
    let r = review(0.0, &mut |_| Ok(()), &mut |round, co| {
        if round < 3 {
            return say(V::Rewrite, "no change yet");
        }
        std::fs::write(co.join("a.patch"), BY_REVIEW).unwrap();
        say(V::Rewrite, "too late")
    });
    assert!(matches!(r.end.unwrap(), PatchReviewEnd::Unsettled(_)));
    assert_eq!(r.verdicts.last().unwrap(), "rolled_back");
    assert_eq!(r.patch, NEW);
}

#[test]
fn a_failed_turn_restores_the_patch_and_stops_with_its_kind() {
    let r = review(0.0, &mut |_| Ok(()), &mut |_, co| {
        std::fs::write(co.join("a.patch"), BY_REVIEW).unwrap();
        Err(("boom".into(), FailureKind::AgentFailed))
    });
    match r.end {
        Err(PipelineResult::Failed { kind, reason, .. }) => {
            assert_eq!(kind, FailureKind::AgentFailed);
            assert!(
                reason.contains("a.patch") && reason.contains("boom"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(r.patch, NEW);
}

#[test]
fn a_spent_budget_stops_the_package_as_exhausted_with_no_turn() {
    let r = review(10.0, &mut |_| Ok(()), &mut |_, _| {
        panic!("no turn may start")
    });
    match r.end {
        Err(PipelineResult::Failed { kind, reason, .. }) => {
            assert_eq!(kind, FailureKind::Exhausted);
            assert!(reason.contains("no turn is left to review it"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(r.verdicts, ["not_reviewed"]);
}
