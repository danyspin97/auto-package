//! The agent's `blocked` answer on a patch of a RefreshPatches turn, read against
//! quilt: a patch quilt shows applying cleanly is not blocked, and a block on a patch
//! that fails still ends the package before its review.

use super::review::{OLD, REBASED, read, reply, reviewed};
use super::*;
use crate::agent::{Action, PatchReviewVerdict as V};

#[test]
fn a_blocked_patch_that_fails_still_restores_the_unreviewed_rewrite() {
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
    // The next round changes nothing and blocks the same patch again: the round stalls.
    let still_blocked: Script = Box::new(|_, _| {
        let mut out = outcome(Action::NoChange, "");
        out.patch_answers.push(crate::agent::PatchAnswer {
            name: "b.patch".into(),
            status: crate::agent::PatchAnswerStatus::Blocked,
            reason: "r".into(),
        });
        out
    });
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", OLD)],
        vec![blocked, still_blocked],
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
fn a_block_on_a_patch_quilt_applies_cleanly_does_not_end_the_package() {
    // b.patch applies (the fake check reads "clean" as applying), so the agent's
    // block on it is not a reason to stop: the rebased a.patch goes to review.
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
                reason: "Applies cleanly ... listed for completeness".into(),
            });
        }
        out
    });
    let case = reviewed(
        &[("a.patch", OLD), ("b.patch", "clean")],
        vec![blocked],
        vec![reply(V::Accept, "fine")],
    );
    assert_eq!(case.reviews.len(), 1, "{:?}", case.result);
    assert!(case.result.is_ok(), "{:?}", case.result);
}
