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
    assert!(example.contains("%install\n       %{python_expand #\n"));
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
