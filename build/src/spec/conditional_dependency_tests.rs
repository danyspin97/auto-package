use super::parenthesize_conditional_dependencies;

#[test]
fn a_bare_conditional_dependency_gets_its_parentheses() {
    let spec = "Requires:       python-tomli >= 2.0.0 if python-base < 3.11\nRequires:       python-a, python-b if python-base < 3.12\nBuildRequires:  %{python_module tomli >= 2.0.0 if %python-base < 3.11}\nRequires:       (python-x if python-base < 3.11)\nRequires:       python-plain >= 1\n# Requires: a if b\n";
    let out = parenthesize_conditional_dependencies(spec);
    assert_eq!(
        out,
        "Requires:       (python-tomli >= 2.0.0 if python-base < 3.11)\nRequires:       python-a, (python-b if python-base < 3.12)\nBuildRequires:  %{python_module tomli >= 2.0.0 if %python-base < 3.11}\nRequires:       (python-x if python-base < 3.11)\nRequires:       python-plain >= 1\n# Requires: a if b\n"
    );
    assert_eq!(parenthesize_conditional_dependencies(&out), out);
}
