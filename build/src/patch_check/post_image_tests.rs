//! The added-lines match of [`super::post_image::added_in_file`], on the Kivy
//! case (a comment the new sources have between a context line and the change)
//! and the refusals that keep the rule strict.

use super::post_image::added_in_file;
use super::present::hunks;

/// Whether the one hunk of `diff` (applied with `-p1`) is in `file`.
fn in_file(file: &str, diff: &str) -> bool {
    let lines: Vec<String> = file.lines().map(String::from).collect();
    let parsed = hunks(diff, 1);
    assert_eq!(parsed.len(), 1);
    added_in_file(&lines, &parsed[0])
}

/// The hunk of the carried `pytest_compatibility.patch` for kivy 2.3.1.
const KIVY_DIFF: &str = "--- a/kivy/tests/common.py\n+++ b/kivy/tests/common.py\n@@ -498,7 +498,7 @@\n         if kivy_eventloop == 'asyncio':\n             try:\n                 import pytest_asyncio\n-                return pytest.mark.asyncio(pytest_asyncio.fixture(func))\n+                return pytest.mark.asyncio(func)\n             except ImportError:\n                 return pytest.mark.skip(\n                     reason='KIVY_EVENTLOOP == \"asyncio\" but '\n";

/// The same lines of kivy 2.3.2's `kivy/tests/common.py`, with the comment line
/// the new sources have between `import pytest_asyncio` and the return.
const KIVY_NEW: &str = "        if kivy_eventloop == 'asyncio':\n            try:\n                import pytest_asyncio\n                # Mark the test itself; pytest 9 rejects marks on fixtures.\n                return pytest.mark.asyncio(func)\n            except ImportError:\n                return pytest.mark.skip(\n                    reason='KIVY_EVENTLOOP == \"asyncio\" but '\n                           '\"pytest-asyncio\" is not installed')(func)\n";

const KIVY_COMMENT: &str =
    "                # Mark the test itself; pytest 9 rejects marks on fixtures.\n";

#[test]
fn the_kivy_hunk_is_in_the_new_sources_past_a_comment() {
    assert!(in_file(KIVY_NEW, KIVY_DIFF));
    // Without the comment the same lines are one run, and still in.
    let plain = KIVY_NEW.replace(KIVY_COMMENT, "");
    assert!(in_file(&plain, KIVY_DIFF));
}

#[test]
fn a_hunk_whose_added_line_is_missing_is_not_in_the_file() {
    let other = KIVY_NEW.replace(
        "return pytest.mark.asyncio(func)",
        "return pytest.mark.asyncio(wrapper(func))",
    );
    assert!(!in_file(&other, KIVY_DIFF));
}

#[test]
fn a_removed_line_still_near_the_match_refuses_the_hunk() {
    // The removed line, with the same indentation, stands just above the match
    // (a line above the match breaks no run: the match starts at `if`).
    let kept = KIVY_NEW.replace(
        "        if kivy_eventloop == 'asyncio':\n",
        "                return pytest.mark.asyncio(pytest_asyncio.fixture(func))\n        if kivy_eventloop == 'asyncio':\n",
    );
    assert!(!in_file(&kept, KIVY_DIFF));
    // The same line at another indentation is not the removed line.
    let other_indent = KIVY_NEW.replace(
        "        if kivy_eventloop == 'asyncio':\n",
        "            return pytest.mark.asyncio(pytest_asyncio.fixture(func))\n        if kivy_eventloop == 'asyncio':\n",
    );
    assert!(in_file(&other_indent, KIVY_DIFF));
}

#[test]
fn a_line_of_code_between_the_context_and_the_change_refuses_the_hunk() {
    let code = KIVY_NEW.replace(KIVY_COMMENT, "                settings = load_settings()\n");
    assert!(!in_file(&code, KIVY_DIFF));
}

#[test]
fn up_to_three_comment_lines_may_stand_between_the_context_and_the_change() {
    let three = KIVY_NEW.replace(
        KIVY_COMMENT,
        "                # one\n                # two\n                # three\n",
    );
    assert!(in_file(&three, KIVY_DIFF));
    let four = KIVY_NEW.replace(
        KIVY_COMMENT,
        "                # one\n                # two\n                # three\n                # four\n",
    );
    assert!(!in_file(&four, KIVY_DIFF));
}

#[test]
fn added_lines_must_be_one_block_even_with_a_comment_between_them() {
    let diff =
        "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1,2 @@\n-a\n+first_added = 1;\n+second_added = 2;\n";
    assert!(in_file("first_added = 1;\nsecond_added = 2;\n", diff));
    assert!(!in_file(
        "first_added = 1;\n# between\nsecond_added = 2;\n",
        diff
    ));
}

#[test]
fn a_pure_addition_needs_its_block_and_may_be_kept_apart_from_its_context() {
    let diff = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n before_line = 1;\n+added_line = 2;\n after_line = 3;\n";
    assert!(!in_file("before_line = 1;\nafter_line = 3;\n", diff));
    assert!(in_file(
        "before_line = 1;\n# note\nadded_line = 2;\nafter_line = 3;\n",
        diff
    ));
}

#[test]
fn an_added_code_line_matches_ignoring_its_leading_and_trailing_whitespace() {
    let diff =
        "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-old_value_here = 1;\n+new_value_here = 2;\n";
    assert!(in_file("    new_value_here = 2;   \n", diff));
    assert!(!in_file("new_value_here = 3;\n", diff));
}
