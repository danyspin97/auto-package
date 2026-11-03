use super::rpmlintrc_guard::{is_rpmlintrc, rpmlintrc_problems};

#[test]
fn a_filter_with_a_reason_line_above_it_is_accepted() {
    let text = "# reason: vendor binary, no hash section\n\
                addFilter(\"missing-hash-section.*libfoo.so\")\n\
                \n\
                # reason: other\n\
                addFilter(\"x\")\n";
    assert!(rpmlintrc_problems(None, text).is_empty());
    assert!(rpmlintrc_problems(None, "# nothing to filter\n").is_empty());
}

#[test]
fn a_filter_without_a_reason_line_is_a_problem() {
    for text in [
        "addFilter(\"x\")\n",
        "# vendor binary\naddFilter(\"x\")\n",
        "# reason: a\n\naddFilter(\"x\")\n",
    ] {
        let problems = rpmlintrc_problems(None, text);
        assert_eq!(problems.len(), 1, "{text}: {problems:?}");
        assert!(problems[0].contains("# reason:"), "{problems:?}");
    }
}

#[test]
fn a_line_that_is_not_a_filter_a_comment_or_blank_is_a_problem() {
    let text = "from Config import *\n# reason: a\naddFilter(\"x\")\nsetBadness('x', 0)\n";
    let problems = rpmlintrc_problems(None, text);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("line 1"));
    assert!(problems[1].contains("line 4"));
}

#[test]
fn both_file_name_forms_are_rpmlintrc_files() {
    assert!(is_rpmlintrc("python-foo-rpmlintrc"));
    assert!(is_rpmlintrc("foo.rpmlintrc"));
    assert!(!is_rpmlintrc("foo.spec"));
}

#[test]
fn only_the_lines_an_edit_adds_are_judged() {
    let old = "from Config import *\n# one reason for both\naddFilter(\"a\")\naddFilter(\"b\")\nsetBadness('x', 0)\n";
    // Removing a filter leaves the legacy shape alone.
    let removed =
        "from Config import *\n# one reason for both\naddFilter(\"a\")\nsetBadness('x', 0)\n";
    assert!(rpmlintrc_problems(Some(old), removed).is_empty());
    // A new filter without a reason is named, and only it.
    let added = format!("{old}addFilter(\"c\")\n");
    let problems = rpmlintrc_problems(Some(old), &added);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("line 6") && problems[0].contains("addFilter(\"c\")"));
}

#[test]
fn a_reason_must_have_text_be_directly_above_and_be_the_edits_own() {
    // An empty reason is no reason.
    let p = rpmlintrc_problems(None, "# reason:\naddFilter(\"x\")\n");
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].contains("empty"), "{p:?}");
    // One to three reason lines directly above are fine.
    assert!(
        rpmlintrc_problems(
            None,
            "# reason: a\n# reason: b\n# reason: c\naddFilter(\"x\")\n"
        )
        .is_empty()
    );
    // A filter slipped under the reason of the old filter below it borrows it.
    let old = "# reason: r\naddFilter(\"a\")\n";
    let new = "# reason: r\naddFilter(\"b\")\naddFilter(\"a\")\n";
    let p = rpmlintrc_problems(Some(old), new);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].contains("already had"), "{p:?}");
    // The same old filter added again, elsewhere, without a reason is added.
    let again = "# reason: r\naddFilter(\"a\")\naddFilter(\"a\")\n";
    assert_eq!(rpmlintrc_problems(Some(old), again).len(), 1);
}

#[test]
fn a_filter_may_span_lines_and_an_open_one_is_named() {
    let ok = "# reason: r\naddFilter(\n  \"x\"\n  \"y\")\n";
    assert!(
        rpmlintrc_problems(None, ok).is_empty(),
        "{:?}",
        rpmlintrc_problems(None, ok)
    );
    let open = "# reason: r\naddFilter(\"x\"\n";
    let p = rpmlintrc_problems(None, open);
    assert!(p.iter().any(|m| m.contains("never closed")), "{p:?}");
}
