use super::*;

#[test]
fn a_licence_term_is_added_to_the_whole_expression() {
    let t = |l: &str| with_license_terms(l.to_string(), &["SUSE-NonFree"]);
    assert_eq!(t("MIT"), "MIT AND SUSE-NonFree");
    assert_eq!(
        t("MIT OR Apache-2.0"),
        "(MIT OR Apache-2.0) AND SUSE-NonFree"
    );
    // Parenthesised at the start is not parenthesised as a whole.
    assert_eq!(
        t("(A OR B) AND C OR D"),
        "((A OR B) AND C OR D) AND SUSE-NonFree"
    );
    assert_eq!(t("(A OR B)"), "(A OR B) AND SUSE-NonFree");
    assert_eq!(t("A AND (B OR C)"), "A AND (B OR C) AND SUSE-NonFree");
    assert_eq!(t("SUSE-NonFree OR MIT"), "SUSE-NonFree OR MIT");
    assert_eq!(t("FIXME"), "FIXME");
    assert!(!has_top_level_or("ORACLE-1.0"));
    assert!(has_top_level_or("A OR(B)"));
}

#[test]
fn only_the_vendor_term_of_a_licence_expression_is_replaced() {
    assert_eq!(
        spdx_expression_for_spec("LicenseRef-NVIDIA-Proprietary"),
        "SUSE-NonFree"
    );
    assert_eq!(
        spdx_expression_for_spec("MIT AND LicenseRef-NVIDIA-Proprietary"),
        "MIT AND SUSE-NonFree"
    );
    assert_eq!(
        spdx_expression_for_spec("(Apache-2.0 OR MIT) AND LicenseRef-Proprietary"),
        "(Apache-2.0 OR MIT) AND SUSE-NonFree"
    );
    assert_eq!(spdx_expression_for_spec("MIT"), "MIT");
    assert_eq!(
        spdx_expression_for_spec("MIT OR Apache-2.0"),
        "MIT OR Apache-2.0"
    );
    // Two vendor terms are one, with no operator left dangling.
    assert_eq!(
        spdx_expression_for_spec("MIT AND LicenseRef-NVIDIA-A AND LicenseRef-NVIDIA-B"),
        "MIT AND SUSE-NonFree"
    );
    assert_eq!(
        spdx_expression_for_spec("LicenseRef-NVIDIA-A AND LicenseRef-NVIDIA-B AND MIT"),
        "SUSE-NonFree AND MIT"
    );
    assert_eq!(
        spdx_expression_for_spec("(LicenseRef-NVIDIA-A OR LicenseRef-Proprietary)"),
        "(SUSE-NonFree)"
    );
}

#[test]
fn a_percent_in_upstream_text_does_not_become_a_macro() {
    let mut r = Release {
        name: "x".into(),
        version: "1".into(),
        summary: Some("100% pure %{python_sitelib} tool".into()),
        homepage: Some("https://example.org/%7Euser".into()),
        license: License::Spdx("MIT".into()),
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: String::new(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    };
    let spec = wheel_binary_skeleton("x", "1", "x-1-py3-none-any.whl", &r, Some(true), None);
    assert!(
        spec.contains("Summary:        100%% pure %%{python_sitelib} tool\n"),
        "{spec}"
    );
    assert!(
        spec.contains("URL:            https://example.org/%%7Euser\n"),
        "{spec}"
    );
    r.summary = Some("two\nlines".into());
    let spec = wheel_binary_skeleton("x", "1", "x-1-py3-none-any.whl", &r, Some(true), None);
    assert!(spec.contains("Summary:        two lines\n"), "{spec}");
    let mono = monorepo_skeleton("x", "1", &[], Some(&r));
    assert!(mono.contains("Summary:        two lines\n"), "{mono}");
}

#[test]
fn ignore_options_are_counted_as_options() {
    let spec = format!(
        "%check\npip install --ignore-installed x\n%pytest {}\n",
        (0..6)
            .map(|i| format!("--ignore=t/{i}.py"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert!(check_quality_problems(&spec).is_empty());
    let seven = format!(
        "%check\n%pytest {} --ignore t/x\n",
        "--ignore-glob=a ".repeat(6)
    );
    assert_eq!(check_quality_problems(&seven).len(), 1);
}

#[test]
fn a_path_given_to_ignore_in_the_next_word_is_not_read() {
    let spec = "%check\n%pytest --ignore tests/gone --deselect tests/x.py::t tests/real\n";
    assert_eq!(
        missing_check_paths(spec, &|_| false),
        vec!["tests/real".to_string()]
    );
}
