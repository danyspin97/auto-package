use super::*;

/// The RPM name the pipeline gives a PyPI project (`rpm_name` lowercases it).
fn rpm(pypi: &str) -> String {
    format!("python-{}", pypi.to_lowercase())
}

/// The four real PyPI pairs that differ only in one separator. Each is two
/// projects, so each member must find only its own package.
const PAIRS: [(&str, &str); 4] = [
    ("python-whois", "pythonwhois"),
    ("junit-xml", "junitxml"),
    ("retry-requests", "retryrequests"),
    ("uri-template", "uritemplate"),
];

#[test]
fn a_separator_difference_is_not_the_same_project() {
    for (a, b) in PAIRS {
        assert_ne!(project_key(a), project_key(b), "{a} and {b}");
        assert_ne!(
            project_key(&rpm(a)),
            project_key(&rpm(b)),
            "the OBS names of {a} and {b}"
        );
    }
    // Separators that PyPI treats alike are the same project.
    assert_eq!(project_key("pydantic_core"), project_key("pydantic-core"));
    assert_eq!(project_key("ruamel.yaml"), project_key("ruamel-yaml"));
    assert_eq!(project_key("PyYAML"), project_key("pyyaml"));
}

#[test]
fn each_member_of_a_pair_finds_its_own_package_when_both_are_listed() {
    for (a, b) in PAIRS {
        let listed = [rpm(a), rpm(b)];
        let names = || listed.iter().map(String::as_str);
        assert_eq!(
            find_listed(&rpm(a), names()),
            Listed::Found(listed[0].as_str()),
            "{a}"
        );
        assert_eq!(
            find_listed(&rpm(b), names()),
            Listed::Found(listed[1].as_str()),
            "{b}"
        );
    }
}

#[test]
fn a_package_is_not_found_under_its_separator_twin() {
    for (a, b) in PAIRS {
        // Only the other project is listed: the requested one is absent.
        assert_eq!(
            find_listed(&rpm(a), [rpm(b).as_str()]),
            Listed::Absent,
            "{a} with only {b} listed"
        );
        assert_eq!(
            find_listed(&rpm(b), [rpm(a).as_str()]),
            Listed::Absent,
            "{b} with only {a} listed"
        );
    }
}

#[test]
fn a_case_variant_is_found_when_it_is_the_only_one() {
    assert_eq!(
        find_listed("python-authlib", ["python-Authlib"]),
        Listed::Found("python-Authlib")
    );
}

#[test]
fn a_separator_variant_is_found_when_it_is_the_only_one() {
    assert_eq!(
        find_listed("python-pydantic-core", ["python-pydantic_core"]),
        Listed::Found("python-pydantic_core")
    );
}

#[test]
fn the_exact_spelling_wins_over_a_separator_variant() {
    assert_eq!(
        find_listed(
            "python-pydantic-core",
            ["python-pydantic_core", "python-pydantic-core"]
        ),
        Listed::Found("python-pydantic-core")
    );
}

#[test]
fn a_case_variant_wins_over_a_separator_variant() {
    assert_eq!(
        find_listed(
            "python-pydantic-core",
            ["python-pydantic.core", "python-Pydantic-Core"]
        ),
        Listed::Found("python-Pydantic-Core")
    );
}

#[test]
fn separator_variants_with_no_exact_or_case_match_are_ambiguous() {
    assert_eq!(
        find_listed(
            "python-pydantic-core",
            ["python-pydantic_core", "python-pydantic.core"]
        ),
        Listed::Ambiguous(vec!["python-pydantic.core", "python-pydantic_core"]),
        "sorted, whatever the listing order"
    );
    assert_eq!(
        find_listed(
            "python-pydantic-core",
            ["python-pydantic.core", "python-pydantic_core"]
        ),
        Listed::Ambiguous(vec!["python-pydantic.core", "python-pydantic_core"]),
    );
}

#[test]
fn case_variants_of_one_spelling_prefer_the_lowercase_one() {
    assert_eq!(
        find_listed("python-FOO", ["python-Foo", "python-foo"]),
        Listed::Found("python-foo")
    );
    assert_eq!(
        find_listed("python-foo", ["python-Foo", "python-FOO"]),
        Listed::Found("python-FOO"),
        "no lowercase spelling listed: the smallest"
    );
}

#[test]
fn an_unrelated_name_is_absent() {
    assert_eq!(
        find_listed("python-yaml", ["python-pyyaml", "python-pydantic_core"]),
        Listed::Absent
    );
}

#[test]
fn fuzzy_key_still_drops_separators_for_its_own_callers() {
    // Kept for the callers that want the looser match; it is not an identity key.
    assert_eq!(
        fuzzy_key(&rpm("pythonwhois")),
        fuzzy_key(&rpm("python-whois"))
    );
    assert_ne!(
        project_key(&rpm("pythonwhois")),
        project_key(&rpm("python-whois"))
    );
}

#[test]
fn obs_name_reports_an_ambiguous_listing_instead_of_choosing() {
    let listing: std::collections::HashSet<String> =
        ["python-pydantic_core", "python-pydantic.core"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    assert!(obs_name(&listing, "python-pydantic-core").is_err());
    let listing: std::collections::HashSet<String> = ["python-pydantic_core"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        obs_name(&listing, "python-pydantic-core").unwrap(),
        "python-pydantic_core"
    );
}
