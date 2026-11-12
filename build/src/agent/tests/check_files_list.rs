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
        "`Installed (but unpackaged) file(s)`, `File listed twice`, `File not found` or `directories not owned by a package` (every `- /path` line under it) entries",
        "check the whole list in the log against the spec again",
    ] {
        assert!(prompt.contains(want), "the fix turn lacks {want:?}");
    }
    // The %check reading order is still there: the task text was moved, not cut.
    assert!(prompt.contains("Read `conventions/10.md` and `conventions/11.md` first"));
}

#[test]
fn an_unowned_directory_is_fixed_on_every_path_with_the_directory_rule() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.task = Task::FixAfterBuild;
    let prompt = build_turn_prompt(&req);
    for want in [
        "`directories not owned by a package` (every `- /path` line under it)",
        "Read `conventions/109.md` first: each new directory needs its own `%dir`",
    ] {
        assert!(prompt.contains(want), "the fix turn lacks {want:?}");
    }
    // The rule the fix turn points to is in convention 109 (index 108).
    let rule = crate::conventions_table::CONVENTIONS[108].1;
    assert!(rule.contains("Every directory the package creates is owned exactly once."));
    assert!(rule.contains("%{_datadir}/wayland-sessions/"));
    let flat = rule.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("directly above the first %files entry"));
    assert!(flat.contains("takes no comment"));
    // The pre-build review of a non-Python package widens the same rule and the every-path rule.
    let review = crate::agent::review_prompt_non_python();
    assert!(review.contains("every new directory the spec installs into"));
    assert!(review.contains("Fix EVERY path the post-build check lists in one turn."));
}

#[test]
fn the_whole_list_rule_is_not_in_a_review_turn() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let mut req = request(ws, &[]);
    req.task = Task::Review;
    let prompt = build_turn_prompt(&req);
    assert!(!prompt.contains("Fix every `%files` error of the log"));
}
