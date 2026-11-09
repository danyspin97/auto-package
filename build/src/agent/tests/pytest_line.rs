//! Convention 7 tells the agent what the tool does to a pytest BuildRequires.

use super::prompt::index_line;

#[test]
fn convention_7_says_the_tool_removes_an_unused_pytest_line_and_reports_it() {
    let text = crate::conventions_table::CONVENTIONS[6].1;
    assert!(text.starts_with("7. A new Python spec"));
    let at = text
        .find("The tool removes `BuildRequires: %{python_module pytest}`")
        .expect("convention 7 has the pytest paragraph");
    let rest = &text[at..];
    // The paragraph is wrapped in the file: compare it with its line breaks folded.
    let para = rest[..rest.find("\n   - ").unwrap_or(rest.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for want in [
        "when `%check` has no pytest line and no test imports pytest",
        "run them with `%pytest` (not `%pyunittest`)",
        "add the BuildRequires in the same edit",
        "the tool reports any line it removes in the next turn's context",
    ] {
        assert!(para.contains(want), "the pytest paragraph lacks {want:?}");
    }
    assert!(
        index_line(7).contains("the notes say the tool removed a pytest BuildRequires"),
        "index line 7 lacks the trigger: {}",
        index_line(7)
    );
}
