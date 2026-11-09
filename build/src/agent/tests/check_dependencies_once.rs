//! Convention 7 asks for every test dependency in the first `%check` edit, and
//! for an addopts option whose plugin is not packaged to be cleared.

use super::prompt::index_line;

#[test]
fn convention_7_lists_the_test_dependencies_before_the_first_check_edit() {
    let text = crate::conventions_table::CONVENTIONS[6].1;
    let at = text
        .find("**Test dependencies in the first `%check` edit")
        .expect("convention 7 has the test-dependency paragraph");
    let rest = &text[at..];
    let para = rest[..rest.find("\n   - ").unwrap_or(rest.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for want in [
        "Before the first `%check` edit",
        "import",
        "`conftest.py`",
        "`addopts` of `setup.cfg`",
        "`BuildRequires: %{python_module <name>}`",
        "in that same edit",
        "`--cov` is the exception",
        "`%pytest -o addopts=''` (convention 11 (iii))",
    ] {
        assert!(para.contains(want), "the paragraph lacks {want:?}");
    }
    assert!(
        index_line(7).contains("the tests import a module or addopts names a plugin"),
        "index line 7 lacks the trigger: {}",
        index_line(7)
    );
}
