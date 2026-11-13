use crate::pipeline::build_round::{review_baseline, review_wanted};

#[test]
fn a_kept_recipe_is_diffed_against_the_spec_the_run_started_with() {
    // No generated spec is linked for a kept recipe: the run-start spec is the baseline.
    assert_eq!(
        review_baseline(None, Some("Name: foo\n")),
        "Name: foo\n",
        "a lint of a kept recipe diffs against the spec it started with"
    );
}

#[test]
fn the_generated_spec_stays_the_baseline_when_there_is_one() {
    assert_eq!(
        review_baseline(Some("generated\n".into()), Some("kept\n")),
        "generated\n"
    );
}

#[test]
fn without_either_spec_the_baseline_is_empty() {
    assert_eq!(review_baseline(None, None), "");
}

#[test]
fn a_lint_review_runs_on_an_unchanged_spec() {
    // A package without findings: the spec is the one it started with.
    assert!(review_wanted(false, true, true));
}

#[test]
fn an_ordinary_review_needs_a_change_to_look_at() {
    assert!(!review_wanted(false, true, false));
    assert!(review_wanted(false, false, false));
}

#[test]
fn an_empty_spec_is_never_reviewed() {
    assert!(!review_wanted(true, false, true));
    assert!(!review_wanted(true, true, false));
}
