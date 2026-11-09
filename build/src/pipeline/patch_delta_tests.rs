use super::patch_delta::patch_delta;
use super::patch_rules_content::source_only_problems;

/// A one-hunk patch of `file` that adds the lines `added` after one context line.
fn adds(file: &str, added: &[&str]) -> String {
    let mut text = format!(
        "--- foo-1.0/{file}\n+++ foo-1.0/{file}\n@@ -1 +1,{} @@\n x\n",
        added.len() + 1
    );
    for line in added {
        text.push_str(&format!("+{line}\n"));
    }
    text
}

const SKIP: &str = "@pytest.mark.skip(reason=\"needs network\")";

#[test]
fn a_skip_the_old_patch_added_in_another_file_is_carried() {
    // The rebase moved the skip from tests.py to tests/test_spf.py.
    let old = adds("tests.py", &[SKIP]);
    let new = adds("tests/test_spf.py", &[SKIP]);
    let problems = source_only_problems(&patch_delta(Some(&old), &new));
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn a_genuinely_new_skip_is_still_reported() {
    let old = adds("tests.py", &["x = 1"]);
    let new = adds("tests/test_spf.py", &[SKIP]);
    let problems = source_only_problems(&patch_delta(Some(&old), &new));
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("adds a skip to the test") && p.contains("tests/test_spf.py")),
        "{problems:?}"
    );
}

#[test]
fn a_skip_added_twice_where_the_old_patch_added_it_once_is_reported_once() {
    let old = adds("tests.py", &[SKIP]);
    let new = adds("tests/test_spf.py", &[SKIP, SKIP]);
    let added = patch_delta(Some(&old), &new).added;
    assert_eq!(added.len(), 1, "{added:?}");
}

#[test]
fn with_no_old_patch_every_added_line_is_new() {
    let new = adds("tests/test_spf.py", &[SKIP]);
    let problems = source_only_problems(&patch_delta(None, &new));
    assert!(!problems.is_empty());
}
