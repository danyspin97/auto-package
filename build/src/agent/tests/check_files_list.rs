//! A failed build that lists several `%files` errors gets one turn that fixes
//! every listed path, not one error per build.

use super::prompt::request;
use super::*;
use crate::agent::build_prompt::*;

#[test]
fn a_fix_turn_is_told_to_fix_the_whole_files_list_in_one_edit() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.task = Task::FixAfterBuild;
    let prompt = build_turn_prompt(&req);
    for want in [
        "Fix the spec after a failed `osc build`.",
        "Fix every `%files` error of the log in one edit.",
        "`Installed (but unpackaged) file(s)`, `File listed twice` or `File not found`",
        "check the whole list in the log against the spec again",
    ] {
        assert!(prompt.contains(want), "the fix turn lacks {want:?}");
    }
    // The %check reading order is still there: the task text was moved, not cut.
    assert!(prompt.contains("Read `conventions/10.md` and `conventions/11.md` first"));
}

#[test]
fn the_whole_list_rule_is_not_in_a_review_turn() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.task = Task::Review;
    let prompt = build_turn_prompt(&req);
    assert!(!prompt.contains("Fix every `%files` error of the log"));
}
