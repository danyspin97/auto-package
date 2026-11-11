#[test]
fn normalize_is_pep_503() {
    for n in ["foo.bar", "foo--bar", "Foo_Bar"] {
        assert_eq!(super::normalize(n), "foo-bar", "{n}");
    }
}

#[test]
fn found_url_must_be_https() {
    let e = super::validate_found_url("http://github.com/o/r", "test-pkg").unwrap_err();
    assert!(e.contains("https"), "{e}");
}

use super::*;
use serde_json::json;

#[test]
fn org_and_user_pages_are_not_repositories() {
    assert!(!is_repository_url("https://github.com/oatpp"));
    assert!(!is_repository_url("https://github.com/cuda"));
    assert!(!is_repository_url("https://codeberg.org/someone/"));
    assert!(is_repository_url("https://github.com/oatpp/oatpp"));
    assert!(is_repository_url("https://gitlab.gnome.org/GNOME/glib"));
    assert!(is_repository_url(
        "https://gitlab.com/group/subgroup/project"
    ));
    assert!(!is_repository_url("not a url"));
    assert!(!is_repository_url(""));
}

fn hits(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, u)| (n.to_string(), u.to_string()))
        .collect()
}

#[test]
fn exact_matches_rank_before_close_ones_and_unrelated_are_dropped() {
    let h = hits(&[
        ("foo-cpp", "u/foo-cpp"),
        ("LeetCUDA", "u/leet"),
        ("foo", "u/foo-1"),
        ("foo", "u/foo-2"),
        ("cpp-foo", "u/cpp-foo"),
        ("foobar", "u/foobar"),
    ]);
    assert_eq!(
        rank_name_matches(&h, "foo"),
        ["u/foo-1", "u/foo-2", "u/foo-cpp", "u/cpp-foo"]
    );
}

#[test]
fn name_matching_ignores_case_and_separators() {
    let h = hits(&[("Yaml_Cpp", "u/y"), ("yamlcpp", "u/z")]);
    assert_eq!(rank_name_matches(&h, "yaml-cpp"), ["u/y", "u/z"]);
}

fn host_allowed(host: &str) -> bool {
    crate::allowed_hosts::AllowedHosts::empty().allows(host, crate::allowed_hosts::Use::Source)
}

#[test]
fn source_hosts_are_matched_by_domain() {
    assert!(host_allowed("github.com"));
    assert!(host_allowed("pypi.org"));
    assert!(host_allowed("test.pypi.org"));
    assert!(!host_allowed("example.com"));
    assert!(!host_allowed("pypi.org.example.com"));
    assert!(!host_allowed("notpypi.org"));
}

#[test]
fn agent_urls_on_prefix_lookalike_hosts_are_not_trusted() {
    // `gitlab.` followed by anything must not pass as GitLab.
    assert!(host_allowed("gitlab.com"));
    assert!(host_allowed("sub.gitlab.com"));
    assert!(!host_allowed("gitlab.com.attacker.example"));
    assert!(!host_allowed("gitlab.example.org"));
    assert!(!host_allowed("evil-gitlab.com"));
    let err =
        validate_found_url("https://gitlab.com.attacker.example/x/y", "test-pkg").unwrap_err();
    assert!(err.contains("is not allowed"), "{err}");
    let err = validate_found_url("https://gitlab.example.org/x/y", "test-pkg").unwrap_err();
    assert!(err.contains("is not allowed"), "{err}");
    assert!(err.contains("--allow-host gitlab.example.org"), "{err}");
}

#[test]
fn an_unknown_host_is_refused_before_any_request() {
    let err = validate_found_url("https://example.com/pkg-1.0.tar.gz", "test-pkg").unwrap_err();
    assert!(err.contains("--allow-host example.com"), "{err}");
}

#[test]
fn parses_gitlab_and_codeberg_search_responses() {
    let gl = json!([
        {"path": "libfoo", "web_url": "https://gitlab.com/a/libfoo"},
        {"path": "foo", "web_url": "https://gitlab.com/b/foo"},
        {"path": "missing-url"}
    ]);
    assert_eq!(
        parse_gitlab_search(&gl, "foo"),
        ["https://gitlab.com/b/foo"]
    );
    let cb = json!({"ok": true, "data": [
        {"name": "foo", "html_url": "https://codeberg.org/c/foo"},
        {"name": "other", "html_url": "https://codeberg.org/c/other"}
    ]});
    assert_eq!(
        parse_codeberg_search(&cb, "foo"),
        ["https://codeberg.org/c/foo"]
    );
    assert!(parse_gitlab_search(&json!({"message": "rate limited"}), "foo").is_empty());
    assert!(parse_codeberg_search(&json!([]), "foo").is_empty());
}

const LZIP_SPEC: &str = "Name:           lzip
URL:            https://www.nongnu.org/lzip/lzip.html
Source:         https://download.savannah.gnu.org/releases/lzip/%name-%version.tar.gz
Source2:        https://download.savannah.gnu.org/releases/lzip/%name-%version.tar.gz.sig
Source3:        %name.keyring
";

#[test]
fn spec_with_a_savannah_source_yields_a_release_directory() {
    let i = parse_spec_upstream(LZIP_SPEC, "lzip");
    assert!(i.forge_repos.is_empty());
    assert_eq!(
        i.index_candidates,
        ["https://download.savannah.gnu.org/releases/lzip/"]
    );
    assert!(
        i.homepages
            .contains(&"https://www.nongnu.org/lzip/lzip.html".to_string())
    );
}

#[test]
fn spec_with_forge_url_and_archive_source_yields_the_repository() {
    let spec = "URL: https://github.com/oatpp/oatpp/\nSource0: https://github.com/oatpp/oatpp/archive/%{version}.tar.gz#/%{name}-%{version}.tar.gz\n";
    let i = parse_spec_upstream(spec, "oatpp");
    assert_eq!(i.forge_repos, ["https://github.com/oatpp/oatpp"]);
    assert!(i.index_candidates.is_empty());
}

#[test]
fn spec_addresses_with_unexpanded_macros_or_foreign_schemes_are_dropped() {
    let spec = "URL: https://%{host}/x\nSource0: https://example.org/%{subdir}/foo-1.tar.gz\nSource1: ftp://example.org/foo-1.tar.gz\nSource2: foo.tar.gz\nSource3: https://example.org/foo-1.tar.xz\n";
    assert_eq!(parse_spec_upstream(spec, "foo"), DistroInfo::default());
    assert_eq!(parse_spec_upstream("", "foo"), DistroInfo::default());
}

#[test]
fn unconfirmed_when_the_distro_records_an_upstream_no_candidate_matches() {
    let none = DistroInfo::default();
    let c = vec!["https://github.com/neuromorphicsystems/lzip".to_string()];
    assert!(
        !candidates_unconfirmed(&c, &none),
        "distro silent: nothing to contradict"
    );

    let off_forge = DistroInfo {
        forge_repos: vec![],
        homepages: vec!["https://www.nongnu.org/lzip".into()],
        index_candidates: vec![],
    };
    assert!(candidates_unconfirmed(&c, &off_forge));

    let confirmed = DistroInfo {
        forge_repos: vec!["https://github.com/NeuromorphicSystems/lzip".into()],
        homepages: vec!["https://github.com/NeuromorphicSystems/lzip".into()],
        index_candidates: vec![],
    };
    assert!(
        !candidates_unconfirmed(&c, &confirmed),
        "case-insensitive match"
    );
    assert!(candidates_unconfirmed(&[], &off_forge));

    let with_index = DistroInfo {
        forge_repos: vec![],
        homepages: vec!["https://download.savannah.gnu.org/releases/lzip/".into()],
        index_candidates: vec!["https://download.savannah.gnu.org/releases/lzip/".into()],
    };
    let c = vec!["https://download.savannah.gnu.org/releases/lzip".to_string()];
    assert!(
        !candidates_unconfirmed(&c, &with_index),
        "a confirmed index counts"
    );
}

#[test]
fn secondary_forges_keep_exact_names_only() {
    let gl = json!([
        {"path": "lzip-go", "web_url": "https://gitlab.com/a/lzip-go"},
        {"path": "lzip", "web_url": "https://gitlab.com/b/lzip"}
    ]);
    assert_eq!(
        parse_gitlab_search(&gl, "lzip"),
        ["https://gitlab.com/b/lzip"]
    );
}

#[test]
fn finding_is_found_inside_prose_or_a_code_fence() {
    let f = parse_finding(
            "Sure!\n```json\n{\"url\": \"https://ftp.gnu.org/gnu/hello/\", \"reason\": \"listing\"}\n```",
        )
        .unwrap();
    assert_eq!(f.url.as_deref(), Some("https://ftp.gnu.org/gnu/hello/"));
    assert_eq!(parse_finding("{\"url\": null}").unwrap().url, None);
    assert!(parse_finding("no json here").is_none());
    assert!(parse_finding("{not json}").is_none());
}

#[test]
fn finding_skips_braces_in_prose_and_takes_the_last_answer() {
    let f = parse_finding(
            "I checked {savannah}. {\"url\": \"https://download.savannah.gnu.org/releases/foo/\", \"reason\": \"listing\"}",
        )
        .unwrap();
    assert_eq!(
        f.url.as_deref(),
        Some("https://download.savannah.gnu.org/releases/foo/")
    );
    // Two answers: the one at the end wins.
    let f = parse_finding(
            "Maybe {\"url\": \"https://a.example/x\", \"reason\": \"first\"} or {\"url\": \"https://ftp.gnu.org/gnu/hello/\", \"reason\": \"final\"}",
        )
        .unwrap();
    assert_eq!(f.url.as_deref(), Some("https://ftp.gnu.org/gnu/hello/"));
    // Braces inside a string value do not end or split the object.
    let f =
        parse_finding("{\"url\": \"https://github.com/o/r\", \"reason\": \"a {b} c\"}").unwrap();
    assert_eq!(f.reason.as_deref(), Some("a {b} c"));
    // A JSON object without a url key is not an answer.
    assert!(parse_finding("{\"reason\": \"nothing\"}").is_none());
    assert!(parse_finding("I checked {savannah}.").is_none());
}

#[test]
fn validation_rejects_non_http_and_org_pages_without_network() {
    assert!(validate_found_url("ftp://ftp.gnu.org/gnu/hello/", "test-pkg").is_err());
    assert!(validate_found_url("javascript:alert(1)", "test-pkg").is_err());
    assert!(validate_found_url("https://github.com/someorg", "test-pkg").is_err());
    assert!(validate_found_url("", "test-pkg").is_err());
}

#[test]
fn agent_output_failure_shapes_all_yield_none_without_panicking() {
    assert_eq!(interpret_agent_output("", 1, "boom", "test-pkg"), None);
    assert_eq!(interpret_agent_output("not json", 0, "", "test-pkg"), None);
    assert_eq!(
        interpret_agent_output(
            r#"{"is_error":true,"subtype":"error_max_budget_usd","result":"x"}"#,
            1,
            "",
            "test-pkg"
        ),
        None
    );
    assert_eq!(
        interpret_agent_output(r#"{"foo":1}"#, 0, "", "test-pkg"),
        None
    );
    assert_eq!(
        interpret_agent_output(r#"{"result":"nothing here"}"#, 0, "", "test-pkg"),
        None
    );
    assert_eq!(
        interpret_agent_output(
            r#"{"result":"{\"url\": null, \"reason\": \"unknown\"}"}"#,
            0,
            "",
            "test-pkg"
        ),
        None
    );
    // A syntactically fine answer with an unusable URL is rejected.
    assert_eq!(
        interpret_agent_output(
            r#"{"result":"{\"url\": \"ftp://x/y\"}"}"#,
            0,
            "",
            "test-pkg"
        ),
        None
    );
}

#[test]
fn rate_limit_replies_are_recognised() {
    assert!(is_rate_limited(
        r#"{"is_error":true,"result":"API Error: Request rejected (429) · quota"}"#
    ));
    assert!(is_rate_limited(r#"{"api_error_status":429,"result":""}"#));
    assert!(!is_rate_limited(r#"{"result":"{\"url\": null}"}"#));
    assert!(!is_rate_limited("not json"));
}

#[test]
fn agent_lookup_is_off_when_disabled() {
    set_agent_enabled(false);
    assert_eq!(
        agent_find_source("anything-at-all", "test-pkg", &Hints::default()),
        None
    );
    set_agent_enabled(true);
}

#[test]
fn a_repo_search_without_the_agent_never_asks_it() {
    // Only an allowed search, with nothing verified or a distro mismatch, asks.
    assert!(agent_fallback_wanted(true, true, false));
    assert!(agent_fallback_wanted(true, false, true));
    assert!(!agent_fallback_wanted(true, false, false));
    for (none_verified, unconfirmed) in [(true, false), (false, true), (true, true)] {
        assert!(!agent_fallback_wanted(false, none_verified, unconfirmed));
    }
}

#[test]
fn lookups_for_one_name_share_a_slot_and_different_names_do_not() {
    let a = in_flight_slot("zz-slot-probe-a");
    assert!(Arc::ptr_eq(&a, &in_flight_slot("zz-slot-probe-a")));
    assert!(!Arc::ptr_eq(&a, &in_flight_slot("zz-slot-probe-b")));
}

#[test]
fn prompt_names_the_project_and_demands_json_only() {
    let p = agent_prompt("lzip", &Hints::default());
    assert!(p.contains("\"lzip\""));
    assert!(p.contains("savannah"));
    assert!(p.contains("ONLY one JSON object"));
}
