
use super::*;

#[test]
fn many_ignores_are_flagged_and_few_are_not() {
    let many = format!(
        "%check\n%pytest {}\n\n%files\n",
        (0..7)
            .map(|i| format!("--ignore=t/{i}.py"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(check_quality_problems(&many).len(), 1);
    let few = "%check\n%pytest --ignore=a --ignore=b\n";
    assert!(check_quality_problems(few).is_empty());
    // An ignore in another section does not count.
    let other = format!("%install\n{}\n%check\nx\n", "--ignore ".repeat(9));
    assert!(check_quality_problems(&other).is_empty());
}

#[test]
fn pytest_is_dropped_only_when_check_does_not_run_it() {
    let import = "BuildRequires:  %{python_module pytest}\nBuildRequires:  %{python_module pytest-xdist}\n%check\n%python_expand $python -c 'import x'\n\n%files\n";
    let out = drop_unused_pytest_build_requires(import);
    assert!(!out.contains("module pytest}"), "{out}");
    assert!(out.contains("pytest-xdist"));
    let runs = "BuildRequires:  %{python_module pytest}\n%check\n%pytest\n";
    assert_eq!(drop_unused_pytest_build_requires(runs), runs);
    let none = "BuildRequires:  %{python_module pytest}\n%files\n";
    assert_eq!(drop_unused_pytest_build_requires(none), none);
}
