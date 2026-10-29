use super::*;
use crate::agent::Action;
use crate::pipeline::patch_condition::*;

#[test]
fn a_patch_turn_that_finds_nothing_to_change_is_satisfied_not_failed() {
    let blocked = Action::Blocked { why: "no".into() };
    // ffmpeg-8 was already licence-clean: no_change.
    assert!(!patch_turn_failed(
        true,
        TurnEffect::NoChange,
        &Action::NoChange
    ));
    // It edited, or this is not a patch turn.
    assert!(!patch_turn_failed(
        true,
        TurnEffect::Edited,
        &Action::Edited
    ));
    assert!(!patch_turn_failed(false, TurnEffect::NoChange, &blocked));
    // Blocked is a failure to patch, on a spec reviewed earlier too.
    assert!(patch_turn_failed(true, TurnEffect::NoChange, &blocked));
    // python-torch: the wheel switch meets the request.
    assert!(!patch_turn_failed(
        true,
        TurnEffect::SwitchedToWheel,
        &Action::UseUpstreamWheel { why: "x".into() }
    ));
}

/// python-torch was asked eleven times, each in new words, for the same
/// change: the text no longer matters once the package was patched twice.
#[test]
fn a_reworded_request_for_an_already_patched_package_is_settled() {
    let a = hash_spec("Keep torch/utils/benchmark/utils in the main package.");
    assert!(!patch_request_settled(&[], "anything"));
    assert!(patch_request_settled(
        &[a],
        "Keep torch/utils/benchmark/utils in the main package."
    ));
    assert!(!patch_request_settled(
        &[a],
        "Ship benchmark/utils in the main package."
    ));
    assert!(patch_request_settled(
        &[a, a ^ 1],
        "Ship benchmark/utils in the main package."
    ));
}
