use crate::name_clash::*;

fn v(urls: &[&str]) -> Vec<String> {
    urls.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_repo_pypi_does_not_point_at_clashes() {
    let got = clashing_repos(
        "jay",
        Some("https://github.com/someone/jay-py"),
        &v(&["https://github.com/mahkoh/jay"]),
    );
    assert_eq!(got, v(&["https://github.com/mahkoh/jay"]));
}

#[test]
fn pypi_pointing_at_the_repo_is_no_clash() {
    let got = clashing_repos(
        "requests",
        Some("https://github.com/Psf/Requests"),
        &v(&["https://github.com/psf/requests"]),
    );
    assert!(got.is_empty());
}

#[test]
fn pypi_with_no_source_clashes_with_an_exact_repo() {
    let got = clashing_repos("jay", None, &v(&["https://github.com/mahkoh/jay"]));
    assert_eq!(got.len(), 1);
}

#[test]
fn a_differently_named_repo_is_not_a_clash() {
    let got = clashing_repos("jay", None, &v(&["https://github.com/x/jay-bindings"]));
    assert!(got.is_empty());
}

#[test]
fn the_skip_and_failure_warnings_name_the_package_and_the_reason() {
    let skip = non_forge_skip_warning("jay");
    assert!(skip.contains("not a known forge") && skip.contains("jay is not checked"));
    let failed = fetch_failed_warning("jay", "PyPI answered 503");
    assert_eq!(failed, "PyPI answered 503; jay is not checked for a clash");
}

#[test]
fn a_custom_domain_source_skips_the_check() {
    let json = r#"{"info": {"project_urls": {"Source": "https://code.example.org/jay"}, "home_page": null}}"#;
    assert!(skips_for_non_forge_source(None, json));
}

#[test]
fn a_homepage_only_project_is_still_checked() {
    let json = r#"{"info": {"project_urls": {"Homepage": "https://jay.example.org/"}, "home_page": "https://jay.example.org/"}}"#;
    assert!(!skips_for_non_forge_source(None, json));
    // A clash stays for the same-named repository.
    let repos = clashing_repos("jay", None, &v(&["https://github.com/mahkoh/jay"]));
    assert_eq!(repos, v(&["https://github.com/mahkoh/jay"]));
}

#[test]
fn a_forge_source_with_a_custom_homepage_is_unaffected() {
    let json = r#"{"info": {"project_urls": {"Source": "https://github.com/someone/jay-py", "Homepage": "https://jay.example.org/"}, "home_page": null}}"#;
    let source = ap_ecosystem::pypi::source_url_from_json(json);
    assert!(!skips_for_non_forge_source(source.as_deref(), json));
    let repos = clashing_repos(
        "jay",
        source.as_deref(),
        &v(&["https://github.com/mahkoh/jay"]),
    );
    assert_eq!(repos, v(&["https://github.com/mahkoh/jay"]));
}

#[test]
fn a_source_that_redirects_to_the_repository_is_no_clash() {
    // PyPI records the old organisation; the repository moved to `new`.
    let source = comparable_source(
        Some("https://github.com/old/jay"),
        Some("https://github.com/new/jay"),
    );
    let got = clashing_repos(
        "jay",
        source.as_deref(),
        &v(&["https://github.com/new/jay"]),
    );
    assert!(got.is_empty());
}

#[test]
fn a_redirect_to_another_repository_is_still_a_clash() {
    let source = comparable_source(
        Some("https://github.com/old/jay"),
        Some("https://github.com/other/jay"),
    );
    let got = clashing_repos(
        "jay",
        source.as_deref(),
        &v(&["https://github.com/new/jay"]),
    );
    assert_eq!(got, v(&["https://github.com/new/jay"]));
}

#[test]
fn an_unresolved_or_non_forge_redirect_keeps_pypis_url() {
    // No answer: the PyPI URL is compared, as before.
    assert_eq!(
        comparable_source(Some("https://github.com/old/jay"), None).as_deref(),
        Some("https://github.com/old/jay")
    );
    // Redirected off the known forges: not a repository to compare.
    let source = comparable_source(
        Some("https://github.com/old/jay"),
        Some("https://jay.example.org/"),
    );
    assert_eq!(source.as_deref(), Some("https://github.com/old/jay"));
    let got = clashing_repos(
        "jay",
        source.as_deref(),
        &v(&["https://github.com/old/jay"]),
    );
    assert!(got.is_empty());
}

#[test]
fn a_resolved_url_is_compared_as_its_canonical_forge_repository() {
    let source = comparable_source(
        Some("https://github.com/old/jay"),
        Some("https://www.github.com/New/Jay.git/tree/main"),
    );
    assert_eq!(source.as_deref(), Some("https://www.github.com/New/Jay"));
    let got = clashing_repos(
        "jay",
        source.as_deref(),
        &v(&["https://github.com/new/jay"]),
    );
    assert!(got.is_empty());
}

#[test]
fn a_repository_named_by_url_or_flag_is_not_checked() {
    // `build <url>` and `--url` both give a repository: nothing to ask.
    assert!(!applies("jay", true, false, false));
}

#[test]
fn an_nvidia_option_is_not_checked() {
    assert!(!applies("jay", false, true, false));
}

#[test]
fn a_source_for_the_name_is_not_checked() {
    assert!(!applies("jay", false, false, true));
}

#[test]
fn an_nvidia_named_package_is_not_checked() {
    for name in [
        "nvidia-cudnn",
        "cuda-cublas",
        "cuda_nvrtc",
        "NVIDIA",
        "nvidia",
    ] {
        assert!(!applies(name, false, false, false), "{name}");
    }
}

#[test]
fn a_name_with_a_curated_mapping_is_not_checked() {
    // Mapped to a repository by the build, without an `nvidia-` spelling.
    for name in ["nvmath-python", "nvshmem4py", "NVSHMEM4PY"] {
        assert!(!applies(name, false, false, false), "{name}");
    }
}

#[test]
fn a_bare_name_is_checked() {
    assert!(applies("jay", false, false, false));
    // Not NVIDIA's: `nvim` and `nvector` only start with `nv`.
    assert!(applies("nvim", false, false, false));
}

#[test]
fn a_repo_with_a_www_host_is_the_same_as_its_bare_host() {
    let got = clashing_repos(
        "jay",
        Some("https://github.com/someone/jay-py"),
        &v(&["https://www.github.com/mahkoh/jay"]),
    );
    assert_eq!(got, v(&["https://www.github.com/mahkoh/jay"]));
    let same = clashing_repos(
        "jay",
        Some("https://www.github.com/mahkoh/jay"),
        &v(&["https://github.com/mahkoh/jay"]),
    );
    assert!(same.is_empty());
}

#[test]
fn a_git_suffix_and_the_ssh_spelling_name_the_same_repo() {
    let got = clashing_repos(
        "jay",
        Some("git@github.com:mahkoh/jay.git"),
        &v(&["https://github.com/mahkoh/jay.git"]),
    );
    assert!(got.is_empty());
    let got = clashing_repos("jay", None, &v(&["git@github.com:mahkoh/jay.git"]));
    assert_eq!(got, v(&["git@github.com:mahkoh/jay.git"]));
}

#[test]
fn an_index_directory_on_another_host_is_not_a_repository() {
    let got = clashing_repos(
        "jay",
        None,
        &v(&[
            "https://ftp.gnu.org/gnu/jay",
            "https://savannah.gnu.org/projects/jay",
        ]),
    );
    assert!(got.is_empty());
}

#[test]
fn underscores_and_dots_in_the_name_match_dashes_in_the_repo() {
    let got = clashing_repos("jay_bird", None, &v(&["https://github.com/x/jay-bird"]));
    assert_eq!(got.len(), 1);
    let got = clashing_repos(
        "zope.interface",
        None,
        &v(&["https://github.com/x/zope-interface"]),
    );
    assert_eq!(got.len(), 1);
}

#[test]
fn every_exact_name_repo_pypi_does_not_point_at_is_listed() {
    let got = clashing_repos(
        "jay",
        Some("https://github.com/someone/jay-py"),
        &v(&[
            "https://github.com/mahkoh/jay",
            "https://gitlab.com/other/jay",
            "https://github.com/x/jay-bindings",
        ]),
    );
    assert_eq!(
        got,
        v(&[
            "https://github.com/mahkoh/jay",
            "https://gitlab.com/other/jay"
        ])
    );
}

#[test]
fn a_generated_guess_is_not_a_clash_but_an_exact_search_hit_is() {
    use ap_build::pipeline::RepoCandidates;
    let guess = RepoCandidates {
        verified: vec![],
        guessed: v(&[
            "https://github.com/python/jay",
            "https://github.com/jay/jay",
        ]),
        unvouched_search_hits: vec![],
    };
    assert!(clashing_repos("jay", None, &clash_candidates(&guess)).is_empty());
    let hit = RepoCandidates {
        verified: vec![],
        guessed: vec![],
        unvouched_search_hits: v(&["https://github.com/x/jay"]),
    };
    assert_eq!(
        clashing_repos("jay", None, &clash_candidates(&hit)),
        v(&["https://github.com/x/jay"])
    );
}

#[test]
fn the_pypi_line_is_true_with_and_without_a_source() {
    let with = clash_message(
        "jay",
        "1.0",
        Some("https://github.com/someone/jay-py"),
        &v(&["https://github.com/x/jay"]),
    );
    assert!(with.contains("(its source: https://github.com/someone/jay-py)"));
    let without = clash_message("jay", "1.0", None, &v(&["https://github.com/x/jay"]));
    assert!(without.contains("(no source repository on a known forge)"));
    assert!(!without.contains("names no source"));
}

fn request(raw: &str) -> Request {
    parse_request(raw).unwrap()
}

#[test]
fn a_pypi_prefix_is_the_pypi_project_under_the_bare_name() {
    assert_eq!(
        request("pypi:jay"),
        Request {
            name: "jay".into(),
            repo: None,
            pypi: true
        }
    );
    assert_eq!(
        request("jay"),
        Request {
            name: "jay".into(),
            repo: None,
            pypi: false
        }
    );
}

#[test]
fn the_pypi_prefix_is_read_without_regard_to_case() {
    for raw in ["PYPI:Foo", "Pypi:Foo", "pYpI:Foo"] {
        let r = request(raw);
        assert_eq!((r.name.as_str(), r.pypi), ("Foo", true), "{raw}");
    }
    // Not a prefix at all: a bare name that does not start with `pypi:`.
    assert_eq!(request("pypí:x").pypi, false);
}

#[test]
fn a_second_colon_after_the_prefix_is_refused() {
    assert!(parse_request("pypi:pypi:x").is_err());
    assert!(parse_request("PYPI:pypi:x").is_err());
}

#[test]
fn the_stripped_pypi_name_is_a_valid_package_name() {
    assert!(crate::validate_package_name(&request("pypi:jay").name).is_ok());
}

#[test]
fn a_request_names_its_package_under_any_spelling() {
    assert_eq!(requested_name("PYPI:jay"), "jay");
    assert_eq!(requested_name("jay"), "jay");
    assert_eq!(requested_name("https://github.com/mahkoh/jay.git"), "jay");
}

#[test]
fn the_root_text_of_a_pypi_build_is_read_back_as_pypi() {
    assert_eq!(root_text("jay", true).as_deref(), Some("pypi:jay"));
    assert_eq!(root_text("jay", false), None);
    let r = request(&root_text("jay", true).unwrap());
    assert!(r.pypi && r.name == "jay" && r.repo.is_none());
}

#[test]
fn a_repository_url_names_its_repository() {
    let r = request("https://github.com/mahkoh/jay");
    assert_eq!(r.name, "jay");
    assert_eq!(r.repo.as_deref(), Some("https://github.com/mahkoh/jay"));
    assert!(!r.pypi);
}

#[test]
fn pypi_with_a_url_or_no_name_is_refused() {
    assert!(parse_request("pypi:https://github.com/mahkoh/jay").is_err());
    assert!(parse_request("pypi:git@github.com:mahkoh/jay.git").is_err());
    // An empty name is left to the package-name check.
    assert_eq!(request("pypi:").name, "");
}

#[test]
fn the_message_offers_both_choices() {
    let m = clash_message("jay", "1.0", None, &v(&["https://github.com/mahkoh/jay"]));
    assert!(m.contains("build pypi:jay") && m.contains("build https://github.com/mahkoh/jay"));
    assert!(m.contains("type `pypi:jay`"));
}
