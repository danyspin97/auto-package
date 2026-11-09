use super::failure_key::{failure_key, same_failure};

#[test]
fn a_line_number_a_temporary_name_and_a_commit_message_do_not_make_a_new_failure() {
    let first = "osc ci failed: python-chess.changes:23: INVALID DATE \
                 (/tmp/.tmpAb12Cd/pkg/python-chess.changes) -m \"auto-package: 1.0\"";
    let later = "osc ci failed: python-chess.changes:25: INVALID DATE \
                 (/tmp/.tmpZz98Yy/pkg/python-chess.changes) -m \"auto-package: 2.0 notes\"";
    assert!(
        same_failure(first, later),
        "{}\n{}",
        failure_key(first),
        failure_key(later)
    );
}

#[test]
fn a_time_a_hash_and_a_date_do_not_make_a_new_failure() {
    let a = "build failed at 00:00:01 on 2026-11-01 from commit 1a2b3c4d5e6f";
    let b = "build failed at 13:45:59 on 2026-11-02 from commit 9f8e7d6c5b4a";
    assert!(same_failure(a, b));
}

#[test]
fn genuinely_different_failures_are_not_the_same() {
    assert!(!same_failure(
        "needs a human: 2 patch(es) do not apply",
        "nothing provides libfoo-devel"
    ));
    assert!(!same_failure(
        "python-chess.changes:23: INVALID DATE",
        "python-chess.spec:23: %check failed"
    ));
}

#[test]
fn the_key_keeps_the_first_200_characters() {
    let head = "x".repeat(200);
    assert!(same_failure(
        &format!("{head}tail one"),
        &format!("{head}tail two")
    ));
    assert_eq!(failure_key(&"y".repeat(300)).chars().count(), 200);
}
