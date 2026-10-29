use super::*;

const SPEC: &str = "Name: python-x\nVersion: 1.2\nSource0: https://files/x-%{version}.tar.gz\n\
%description\nd\n\n%prep\n%autosetup\n%check\n%pytest tests/test_configs tests/test_configs/a.yml\n";
const PI: &str = "Project-URL: Source, https://github.com/o/x\n";

#[test]
fn resolves_the_repo_and_tag_or_leaves_the_prompt() {
    let found = |r: &str, v: &str| Some(format!("{r}/archive/refs/tags/v{v}.tar.gz"));
    let out = auto_fixture_source(SPEC, &|_| false, PI, "", "1.2", &found).unwrap();
    assert!(
        out.contains(
            "Source1:        https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz#/"
        )
    );
    // The file inside the directory does not add a second wildcard.
    assert!(
        out.contains("--wildcards '*/tests/test_configs/*'\n"),
        "{out}"
    );
    assert_eq!(out.matches("--wildcards").count(), 1);
    assert_eq!(
        auto_fixture_source(SPEC, &|_| false, PI, "", "1.2", &|_, _| None),
        None
    );
    assert_eq!(
        auto_fixture_source(SPEC, &|_| false, "", "", "1.2", &found),
        None
    );
    assert_eq!(
        auto_fixture_source(SPEC, &|_| true, PI, "", "1.2", &found),
        None
    );
}
