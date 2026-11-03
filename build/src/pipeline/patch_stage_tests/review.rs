//! The review of the patches a RefreshPatches turn rewrote: when it runs, what a
//! reviewer may change and how each verdict ends the stage.

use super::*;
use crate::agent::{Action, PatchReviewAnswer, PatchReviewVerdict as V};

/// A patch that does not apply in the fake check.
const OLD: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+old\n";
/// What a rebase makes of it: the code changed, and the fake check applies it.
const REBASED: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+clean\n";
/// What a reviewer makes of the rebase.
const BETTER: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1,2 @@\n-x\n+clean\n+extra\n";

/// A review answer.
pub(super) fn said(verdict: V, reason: &str) -> crate::agent::TurnOutcome {
    let mut out = outcome(Action::NoChange, "");
    out.patch_review = Some(PatchReviewAnswer {
        verdict,
        reason: reason.into(),
        problems: Vec::new(),
    });
    out
}

type Reply = Result<crate::agent::TurnOutcome, (String, FailureKind)>;

/// An agent turn that rebases `name` to `body`.
fn rebase(
    name: &'static str,
    body: &'static str,
) -> Box<dyn FnMut(&Path, &Path) -> crate::agent::TurnOutcome> {
    Box::new(move |checkout, _| {
        std::fs::write(checkout.join(name), body).unwrap();
        outcome(Action::Edited, &format!("Refresh {name}"))
    })
}

/// A run whose agent turns come from `agent` and review turns from `reviews`
/// (each given the checkout and the review's context).
fn reviewed(
    patches: &[(&str, &str)],
    agent: Vec<Script>,
    mut reviews: Vec<Box<dyn FnMut(&Path, &str) -> Reply>>,
) -> Case {
    let mut agent = std::collections::VecDeque::from(agent);
    let mut at = 0;
    run_reviewed(
        patches,
        false,
        10,
        &mut |ctx, _| {
            let mut script = agent.pop_front().expect("more agent turns than scripted");
            Ok(script(&ctx.checkout_dir, &ctx.spec_path))
        },
        &mut |ctx, context| {
            let r = reviews
                .get_mut(at)
                .expect("more review turns than scripted")(
                &ctx.checkout_dir, context
            );
            at += 1;
            r
        },
    )
}

fn reply(verdict: V, reason: &'static str) -> Box<dyn FnMut(&Path, &str) -> Reply> {
    Box::new(move |_, _| Ok(said(verdict, reason)))
}

fn read(case: &Case, name: &str) -> String {
    std::fs::read_to_string(case.checkout.join(name)).unwrap()
}

fn failed(result: &Result<(), PipelineResult>) -> (FailureKind, String) {
    match result {
        Err(PipelineResult::Failed { kind, reason, .. }) => (*kind, reason.clone()),
        other => panic!("{other:?}"),
    }
}

fn verdicts(case: &Case) -> Vec<&str> {
    case.verdicts.iter().map(|v| v.1.as_str()).collect()
}

#[test]
fn a_rebase_that_changes_a_line_is_reviewed_once_and_accepted() {
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED)],
        vec![reply(V::Accept, "same intent")],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.reviews.len(), 1);
    assert!(case.reviews[0].contains("Review round 1 of at most 3"));
    assert!(case.reviews[0].contains("a.patch"), "{}", case.reviews[0]);
    assert_eq!(verdicts(&case), ["accepted"]);
    assert_eq!(
        bullets(&case.changes, "Refresh a.patch"),
        1,
        "{}",
        case.changes
    );
    assert_eq!(
        case.changes.matches("review").count(),
        0,
        "{}",
        case.changes
    );
    assert!(
        case.notes.contains("## PatchReview — a.patch — round 1"),
        "{}",
        case.notes
    );
    assert!(
        case.notes.contains("review round 1: accepted: same intent"),
        "{}",
        case.notes
    );
}

#[test]
fn a_fuzz_refresh_starts_no_review() {
    // The pipeline's own refresh before any turn, and one after a turn.
    let case = run(&[("a.patch", "FUZZ")], false, 10, vec![]);
    assert!(case.result.is_ok(), "{:?}", case.result);
    let case = run(
        &[("a.patch", "old hunks")],
        false,
        10,
        vec![
            Box::new(|checkout, _| {
                std::fs::write(checkout.join("a.patch"), "FUZZ").unwrap();
                outcome(Action::Edited, "Refresh a.patch")
            }),
            Box::new(|_, _| outcome(Action::NoChange, "")),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(read(&case, "a.patch"), "clean\n");
}

#[test]
fn a_rewritten_patch_that_ends_the_loop_with_fuzz_is_reviewed_refreshed() {
    let fuzzy: &'static str = Box::leak(format!("{REBASED}# FUZZ\n").into_boxed_str());
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![
            rebase("a.patch", fuzzy),
            Box::new(|_, _| outcome(Action::NoChange, "")),
        ],
        vec![reply(V::Accept, "fine")],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.reviews.len(), 1);
    assert_eq!(read(&case, "a.patch"), "clean\n");
    assert_eq!(verdicts(&case), ["accepted"]);
}

#[test]
fn a_rewrite_that_applies_is_what_the_next_round_reviews() {
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED)],
        vec![
            Box::new(|co, _| {
                std::fs::write(co.join("a.patch"), BETTER).unwrap();
                Ok(said(V::Rewrite, "added the missing line"))
            }),
            reply(V::Accept, "now right"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(verdicts(&case), ["rewritten", "accepted"]);
    assert!(case.reviews[1].contains("+extra"), "{}", case.reviews[1]);
    assert_eq!(read(&case, "a.patch"), BETTER);
    assert_eq!(case.rewrote, ["a.patch"]);
}

#[test]
fn a_rewrite_that_does_not_apply_is_rolled_back_and_told() {
    let broken = REBASED.replace("+clean", "+broken");
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED)],
        vec![
            Box::new(move |co, _| {
                std::fs::write(co.join("a.patch"), &broken).unwrap();
                Ok(said(V::Rewrite, "try"))
            }),
            reply(V::Accept, "the writer's was right"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(verdicts(&case), ["rolled_back", "accepted"]);
    assert_eq!(read(&case, "a.patch"), REBASED);
    assert!(
        case.reviews[1].contains("Your last rewrite was rolled back"),
        "{}",
        case.reviews[1]
    );
    assert!(
        case.reviews[1].contains("does not apply"),
        "{}",
        case.reviews[1]
    );
}

#[test]
fn a_reviewer_that_edits_the_spec_and_another_patch_is_put_back_and_told() {
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", "clean")],
        vec![rebase("a.patch", REBASED)],
        vec![
            Box::new(|co, _| {
                std::fs::write(co.join("foo.spec"), "Name: evil\n").unwrap();
                std::fs::write(co.join("b.patch"), "evil").unwrap();
                Ok(said(V::Rewrite, "changed things"))
            }),
            reply(V::Accept, "fine"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(read(&case, "b.patch"), "clean");
    assert!(case.spec.starts_with("Name: foo"), "{}", case.spec);
    assert!(
        case.reviews[1]
            .contains("only a.patch may change; your edit to b.patch, foo.spec was rolled back"),
        "{}",
        case.reviews[1]
    );
}

#[test]
fn a_reject_puts_the_patch_back_and_the_next_rebase_is_told_the_reason() {
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED), rebase("a.patch", BETTER)],
        vec![
            reply(V::Reject, "drops a hunk"),
            reply(V::Accept, "now right"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 2);
    assert!(
        case.contexts[1].contains("the patch review rejected a.patch: drops a hunk"),
        "{}",
        case.contexts[1]
    );
    assert!(
        case.contexts[1].contains("a.patch: FAILS"),
        "the patch is back to OBS bytes: {}",
        case.contexts[1]
    );
    assert_eq!(verdicts(&case), ["rejected", "accepted"]);
    assert_eq!(read(&case, "a.patch"), BETTER);
}

#[test]
fn a_reject_in_the_last_round_needs_a_maintainer_with_the_patch_restored() {
    let agent: Vec<Script> = (0..4).map(|_| rebase("a.patch", REBASED)).collect();
    let reviews = (0..4).map(|_| reply(V::Reject, "still wrong")).collect();
    let case = reviewed(&[("a.patch", OLD)], agent, reviews);
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(
        reason.contains("a.patch") && reason.contains("rejected") && reason.contains("maintainer"),
        "{reason}"
    );
    assert_eq!(case.contexts.len(), 4);
    assert_eq!(read(&case, "a.patch"), OLD);
}

#[test]
fn a_reviewer_that_gives_up_needs_a_maintainer() {
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED)],
        vec![Box::new(|_, _| {
            Ok(outcome(
                Action::Blocked {
                    why: "cannot follow it".into(),
                },
                "",
            ))
        })],
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(
        reason.contains("the patch review gave up on a.patch: cannot follow it"),
        "{reason}"
    );
    assert_eq!(read(&case, "a.patch"), OLD);
    assert!(case.notes.contains("review stopped"), "{}", case.notes);
}

#[test]
fn three_rounds_without_an_accept_need_a_maintainer_with_the_patch_restored() {
    let case = reviewed(
        &[("a.patch", OLD)],
        vec![rebase("a.patch", REBASED)],
        (0..3).map(|_| reply(V::Rewrite, "I rewrote it")).collect(),
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(
        reason.contains("did not settle a.patch in 3 rounds"),
        "{reason}"
    );
    assert_eq!(read(&case, "a.patch"), OLD);
}

#[test]
fn a_failed_review_turn_stops_with_its_kind_and_the_patch_restored() {
    for kind in [FailureKind::AgentFailed, FailureKind::Tampered] {
        let case = reviewed(
            &[("a.patch", OLD)],
            vec![rebase("a.patch", REBASED)],
            vec![Box::new(move |co, _| {
                std::fs::write(co.join("a.patch"), BETTER).unwrap();
                Err(("boom".into(), kind))
            })],
        );
        let (got, reason) = failed(&case.result);
        assert_eq!(got, kind);
        assert!(reason.contains("boom"), "{reason}");
        assert_eq!(read(&case, "a.patch"), OLD);
    }
}

#[test]
fn an_accepted_patch_is_not_reviewed_again_after_a_redo() {
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", OLD)],
        vec![
            Box::new(|co, _| {
                std::fs::write(co.join("a.patch"), REBASED).unwrap();
                std::fs::write(co.join("b.patch"), REBASED).unwrap();
                outcome(Action::Edited, "Refresh a.patch")
            }),
            rebase("b.patch", BETTER),
        ],
        vec![
            reply(V::Accept, "a is right"),
            reply(V::Reject, "b is wrong"),
            reply(V::Accept, "b is right"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.reviews.len(), 3, "a once, b twice");
    assert!(case.reviews[0].contains("Patch: a.patch"));
    assert!(case.reviews[1].contains("Patch: b.patch"));
    assert!(case.reviews[2].contains("Patch: b.patch"));
}

#[test]
fn a_patch_proven_already_applied_and_removed_is_not_reviewed() {
    let case = reviewed(
        &[("a.patch", "UPSTREAM"), ("b.patch", OLD)],
        vec![rebase("b.patch", REBASED)],
        vec![reply(V::Accept, "fine")],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.reviews.len(), 1);
    assert!(case.reviews[0].contains("Patch: b.patch"));
    assert!(!case.spec.contains("a.patch"), "{}", case.spec);
}

#[test]
fn a_new_patch_is_reviewed_as_new_and_a_reject_has_no_redo() {
    let new_fix = "Subject: Fix the build with gcc 15\nDescription: gcc 15 rejects the implicit int in foo.c, so the build stops.\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+clean\n";
    let adder: Script = Box::new(move |checkout, spec_path| {
        std::fs::write(checkout.join("b.patch"), "clean, rebased").unwrap();
        std::fs::write(checkout.join("new-fix.patch"), new_fix).unwrap();
        let spec = std::fs::read_to_string(spec_path).unwrap().replacen(
            "\n%prep",
            "\n# PATCH-FIX-UPSTREAM new-fix.patch -- gcc 15 build\nPatch1: new-fix.patch\n%prep",
            1,
        );
        std::fs::write(spec_path, spec).unwrap();
        outcome(Action::Edited, "Add new-fix.patch: gcc 15 build")
    });
    let case = reviewed(
        &[("b.patch", "old hunks")],
        vec![adder],
        vec![reply(V::Reject, "not needed")],
    );
    assert!(
        case.reviews[0].contains("none: the patch is new"),
        "{}",
        case.reviews[0]
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(
        reason.contains("new-fix.patch") && reason.contains("new in this update"),
        "{reason}"
    );
    assert_eq!(case.contexts.len(), 1, "no redo of a new patch");
}
