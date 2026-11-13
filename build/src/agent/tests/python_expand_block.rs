//! Convention 19 carries the braced `%{python_expand #` example that a spec
//! mangled three times with `Unterminated {`.

use super::prompt::index_line;

#[test]
fn convention_19_has_the_braced_python_expand_worked_example() {
    let text = crate::conventions_table::CONVENTIONS[18].1;
    assert!(text.starts_with("19. RPM macros use single"));
    let at = text
        .find("Worked example, `%install`")
        .expect("convention 19 has the worked example");
    let example = &text[at..];
    assert!(example.contains(
        "%install\n       # the wheel ships test files that no flavor should package\n       %{python_expand #\n"
    ));
    assert!(example.contains("rm -f %{buildroot}%{$python_sitelib}/<pkg>/conftest.py\n"));
    assert!(
        example.contains("\n       }\n"),
        "the closing brace is alone at column 0"
    );
    assert!(example.contains("`Unterminated {`"));
}

#[test]
fn the_index_routes_unterminated_brace_to_convention_19() {
    let line = index_line(19);
    assert!(line.contains("\"Unterminated {\""), "{line:?}");
}

#[test]
fn convention_19_keeps_any_origin_braced_python_expand_block_as_it_is() {
    let text = crate::conventions_table::CONVENTIONS[18].1;
    let at = text
        .find("119. **An origin's valid braced `%{python_expand ...}` block")
        .expect("convention 19 has the origin-block rule");
    let rule = &text[at..];
    assert!(rule.contains("keep the block as it is and"));
    assert!(rule.contains("Never turn a block into per-line"));
    assert!(rule.contains("never merge two blocks or split one"));
    assert!(rule.contains("never write\n   `%python_expand %fdupes ...`"));
    assert!(rule.contains("`%{python_expand PYTHONPATH=%{buildroot}%{$python_sitelib}`"));
    assert!(rule.contains("do not convert it to the `#` form"));
    assert!(rule.contains("`$python -m pytest tests`"));
    assert!(rule.contains("only its `sed` line changes"));
}

#[test]
fn the_index_routes_an_origin_opener_edit_to_convention_19() {
    let line = index_line(19);
    assert!(line.contains("(119) "), "{line:?}");
}
