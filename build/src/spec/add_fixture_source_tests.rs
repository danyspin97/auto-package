
use super::*;

const SPEC: &str = "Name: python-x\nVersion: 1.2\nSource0: https://files/x-%{version}.tar.gz\n\
Source1: x.rpmlintrc\nBuildRequires: a\n\n%description\nd\n\n%prep\n%autosetup -p1 \\\n  -n x-%{version}\n\
%build\n%check\n%pytest tests/test_configs\n";

#[test]
fn adds_the_next_source_and_the_extract_line() {
    let dirs = vec!["tests/test_configs".to_string()];
    let out = add_fixture_source(
        SPEC,
        "https://github.com/o/x/archive/refs/tags/v1.2.tar.gz",
        "1.2",
        &dirs,
    )
    .unwrap();
    assert!(out.contains(
            "Source1: x.rpmlintrc\nSource2:        https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz#/x-fixtures-v%{version}.tar.gz\nBuildRequires"
        ), "{out}");
    assert!(out.contains(
            "  -n x-%{version}\n# The sdist leaves out tests/test_configs\ntar -xzf %{SOURCE2} --strip-components=1 --wildcards '*/tests/test_configs/*'\n%build"
        ), "{out}");
    // Applied once, no matter how often the check runs.
    assert_eq!(
        add_fixture_source(&out, "https://github.com/o/x/a.tar.gz", "1.2", &dirs),
        None
    );
    assert!(missing_check_path_problems(&out, &|_| false).is_empty());
}

#[test]
fn refuses_what_it_cannot_place() {
    let d = vec!["tests/a".to_string()];
    let u = "https://github.com/o/x/archive/v1.tar.gz";
    assert_eq!(
        add_fixture_source("Name: x\n%prep\n%autosetup\n", u, "1", &d),
        None
    );
    assert_eq!(
        add_fixture_source("Source0: a\n%description\n%prep\nmake\n", u, "1", &d),
        None
    );
    assert_eq!(
        add_fixture_source(SPEC, u, "1", &["../x".to_string()]),
        None
    );
    assert_eq!(add_fixture_source(SPEC, u, "1", &[]), None);
    assert_eq!(add_fixture_source(SPEC, "https://h/a b", "1", &d), None);
}
