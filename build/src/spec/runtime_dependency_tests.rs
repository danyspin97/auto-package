
use super::*;

fn dist(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

/// fastapi-cli: PyPI lists its dependencies, and the generated spec had
/// none of them (the review added them by hand).
#[test]
fn only_unconditional_dependencies_with_a_lower_bound_are_kept() {
    let deps = runtime_dependencies(
        &dist(&[
            "typer>=0.16.0",
            "uvicorn[standard]>=0.15.0",
            "tomli>=2.0.0; python_version < \"3.11\"",
            "fastapi-cloud-cli>=0.1.1; extra == \"standard\"",
            "rich-toolkit==0.14.8",
            "mcp<3.0.0,>=2.0.0",
            "pydantic (>=2,<3)",
            "idna!=3.0",
            "fastapi-cli",
            "Fastapi_CLI>=1",
        ]),
        "fastapi-cli",
    );
    let shown: Vec<String> = deps
        .iter()
        .map(|d| {
            format!(
                "{}{}",
                d.name,
                d.bound
                    .as_deref()
                    .map(|b| format!(" {b}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            "typer >= 0.16.0",
            "uvicorn >= 0.15.0",
            "rich-toolkit >= 0.14.8",
            "mcp >= 2.0.0",
            "pydantic >= 2",
            "idna"
        ]
    );
}

#[test]
fn requires_go_before_the_subpackages_and_are_mirrored_into_build_requires() {
    let spec = "Name: python-x\nBuildRequires:  python-rpm-macros\nBuildRequires:  %{python_module pip}\nRequires:       python-click\n%python_subpackages\n\n%description\nx\n";
    let out = add_runtime_dependencies(spec, &dist(&["click>=8", "rich>=13.0"]), "x");
    assert!(
        out.contains(
            "Requires:       python-click\nRequires:       python-rich >= 13.0\n%python_subpackages"
        ),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module rich >= 13.0}"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module click >= 8}"),
        "{out}"
    );
    // Nothing to add twice.
    assert_eq!(
        add_runtime_dependencies(&out, &dist(&["click>=8", "rich>=13.0"]), "x"),
        out
    );
}
