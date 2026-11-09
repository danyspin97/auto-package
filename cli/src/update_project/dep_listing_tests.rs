use super::dep_listing::listed_spelling;

/// `pkg` looked up among the one listed name `listed`.
fn one(pkg: &str, listed: &str) -> Option<String> {
    listed_spelling(pkg, &[listed]).unwrap().map(str::to_string)
}

#[test]
fn separator_collisions_are_different_projects() {
    // The four real pairs: the separator-blind key made each one match the other.
    for (pkg, other) in [
        ("python-whois", "python-pythonwhois"),
        ("python-junitxml", "python-junit-xml"),
        ("python-retryrequests", "python-retry-requests"),
        ("python-uritemplate", "python-uri-template"),
    ] {
        assert_eq!(one(pkg, other), None, "{pkg} must not match {other}");
        assert_eq!(one(other, pkg), None, "{other} must not match {pkg}");
        assert_eq!(one(pkg, pkg), Some(pkg.to_string()));
    }
}

#[test]
fn case_only_spelling_is_the_same_project() {
    assert_eq!(
        one("python-authlib", "python-Authlib"),
        Some("python-Authlib".to_string())
    );
    assert_eq!(
        one("python-Authlib", "python-authlib"),
        Some("python-authlib".to_string())
    );
}

#[test]
fn separator_spelling_is_the_same_project() {
    assert_eq!(
        one("python-pydantic-core", "python-pydantic_core"),
        Some("python-pydantic_core".to_string())
    );
}

#[test]
fn exact_spelling_wins_among_case_variants() {
    let got = listed_spelling("python-authlib", &["python-Authlib", "python-authlib"]);
    assert_eq!(got, Ok(Some("python-authlib")));
}

#[test]
fn separator_only_variants_are_ambiguous() {
    let got = listed_spelling("python-foo.bar", &["python-foo_bar", "python-foo-bar"]);
    let err = got.unwrap_err();
    assert!(err.contains("python-foo-bar, python-foo_bar"), "{err}");
}

#[test]
fn absent_when_nothing_is_the_same_project() {
    assert_eq!(
        listed_spelling("python-whois", &["python-requests"]),
        Ok(None)
    );
}
