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
        case.contexts[1].contains(
            "the patch review rejected a.patch:\n<untrusted-data source=\"patch review reasons\">"
        ),
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
    // The rejected new patch stays on disk, recorded: a rerun refuses it.
    assert!(case.checkout.join("new-fix.patch").exists());
    assert_eq!(case.unreviewed, ["new-fix.patch"]);
    assert!(case.rerun_refused);
}

#[test]
fn a_stop_restores_every_rewritten_patch_the_review_did_not_reach() {
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", OLD)],
        vec![Box::new(|co, _| {
            std::fs::write(co.join("a.patch"), REBASED).unwrap();
            std::fs::write(co.join("b.patch"), REBASED).unwrap();
            outcome(Action::Edited, "Refresh a.patch")
        })],
        vec![Box::new(|_, _| {
            Ok(outcome(
                Action::Blocked {
                    why: "cannot follow it".into(),
                },
                "",
            ))
        })],
    );
    assert_eq!(failed(&case.result).0, FailureKind::NeedsMaintainer);
    assert_eq!(case.reviews.len(), 1, "b was never reviewed");
    assert_eq!(read(&case, "a.patch"), OLD);
    assert_eq!(read(&case, "b.patch"), OLD);
    assert!(!case.rerun_refused);
}

#[test]
fn a_blocked_patch_with_a_clean_check_still_restores_the_unreviewed_rewrite() {
    let blocked: Script = Box::new(|co, _| {
        std::fs::write(co.join("a.patch"), REBASED).unwrap();
        let mut out = outcome(Action::Edited, "Refresh a.patch");
        for (name, status) in [
            ("a.patch", crate::agent::PatchAnswerStatus::Rebased),
            ("b.patch", crate::agent::PatchAnswerStatus::Blocked),
        ] {
            out.patch_answers.push(crate::agent::PatchAnswer {
                name: name.into(),
                status,
                reason: "r".into(),
            });
        }
        out
    });
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", "clean")],
        vec![blocked],
        vec![],
    );
    assert!(case.result.is_err(), "{:?}", case.result);
    assert!(
        case.reviews.is_empty(),
        "the blocked patch ends the package first"
    );
    assert_eq!(
        read(&case, "a.patch"),
        OLD,
        "an unreviewed rewrite does not stay"
    );
}

#[test]
fn a_patch_with_an_unsafe_name_is_never_reviewed_and_needs_a_maintainer() {
    let case = reviewed(
        &[("a,b.patch", OLD)],
        vec![rebase("a,b.patch", REBASED)],
        vec![],
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(reason.contains("not a plain file name"), "{reason}");
    assert!(case.reviews.is_empty());
    assert_eq!(read(&case, "a,b.patch"), OLD);
}

#[test]
fn a_reject_with_no_round_left_to_redo_it_needs_a_maintainer() {
    let mut agent = std::collections::VecDeque::from(vec![rebase("a.patch", REBASED)]);
    let case = run_reviewed(
        &[("a.patch", OLD)],
        false,
        1,
        &mut |ctx, _| {
            let mut s = agent.pop_front().unwrap();
            Ok(s(&ctx.checkout_dir, &ctx.spec_path))
        },
        &mut |_, _| Ok(said(V::Reject, "wrong")),
    );
    assert_eq!(failed(&case.result).0, FailureKind::NeedsMaintainer);
    assert_eq!(read(&case, "a.patch"), OLD);
}

#[test]
fn a_failed_turn_does_not_claim_or_bullet_a_rewrite_it_puts_back() {
    let case = run_reviewed(
        &[("a.patch", OLD)],
        false,
        10,
        &mut |ctx, _| {
            // The agent rewrote the patch (quilt would prove it applies), then failed.
            std::fs::write(ctx.checkout_dir.join("a.patch"), REBASED).unwrap();
            Err(("boom".into(), FailureKind::AgentFailed))
        },
        &mut |_, _| panic!("no review after a failed turn"),
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::AgentFailed);
    assert!(
        !reason.contains("edits quilt proves apply were kept"),
        "{reason}"
    );
    assert_eq!(read(&case, "a.patch"), OLD);
    assert_eq!(
        bullets(&case.changes, "Refresh a.patch"),
        0,
        "{}",
        case.changes
    );
    assert!(case.notes.contains("a.patch: put back"), "{}", case.notes);
}

/// Records the patches as `agent_turn` does before a real turn.
fn begin(checkout: &Path, spec_path: &Path) {
    let state = checkout.parent().unwrap().parent().unwrap().join("state");
    begin_turn(&Workspace::open(&state, "foo").unwrap(), spec_path).unwrap();
}

#[test]
fn the_turn_record_is_gone_when_the_stage_settled_and_kept_when_it_could_not() {
    // Accepted, then a stop that put the patch back: nothing is left recorded.
    let ok = reviewed(
        &[("a.patch", OLD)],
        vec![Box::new(|co, spec| {
            begin(co, spec);
            std::fs::write(co.join("a.patch"), REBASED).unwrap();
            outcome(Action::Edited, "Refresh a.patch")
        })],
        vec![reply(V::Accept, "fine")],
    );
    assert!(ok.result.is_ok() && !ok.record_left);
    let put_back = reviewed(
        &[("a.patch", OLD)],
        vec![Box::new(|co, spec| {
            begin(co, spec);
            std::fs::write(co.join("a.patch"), REBASED).unwrap();
            outcome(Action::Edited, "Refresh a.patch")
        })],
        vec![Box::new(|_, _| {
            Ok(outcome(Action::Blocked { why: "no".into() }, ""))
        })],
    );
    assert!(put_back.result.is_err() && !put_back.record_left);
    // A new patch that cannot be recorded as unreviewed: the stage stops for a
    // maintainer and the turn record stays.
    let new_fix = "Subject: s\nDescription: a long enough description of the build break.\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+clean\n";
    let adder: Script = Box::new(move |checkout, spec_path| {
        begin(checkout, spec_path);
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
        vec![Box::new(|co, _| {
            // Both places for the record of the new patch are blocked.
            let state = co.parent().unwrap().parent().unwrap().join("state");
            let ws = Workspace::open(&state, "foo").unwrap().dir().to_path_buf();
            std::fs::create_dir_all(ws.join("patch-review/pending")).unwrap();
            let name = ws.file_name().unwrap().to_string_lossy().into_owned();
            std::fs::create_dir_all(
                ws.parent()
                    .unwrap()
                    .join(format!("{name}-patch-review-pending")),
            )
            .unwrap();
            Ok(said(V::Reject, "not needed"))
        })],
    );
    let (kind, reason) = failed(&case.result);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert!(reason.contains("left unreviewed"), "{reason}");
    assert!(
        case.record_left,
        "the record must survive an unsettled stop"
    );
}
