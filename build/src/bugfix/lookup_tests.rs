use super::*;
use serde_json::Value;

const REPO: &str = "https://github.com/o/r";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SEARCH: &str = include_str!("../testdata/bugfix/gh_search_commits.json");
const BEHIND: &str = include_str!("../testdata/bugfix/gh_compare_behind.json");
const DIVERGED: &str = include_str!("../testdata/bugfix/gh_compare_diverged.json");

fn text(urls: &[&str], comments: &[&str]) -> BugText {
    BugText {
        source: "t".into(),
        urls: urls.iter().map(|u| u.to_string()).collect(),
        comments: comments.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn urls_lose_the_punctuation_around_them() {
    assert_eq!(
        urls_in("see (https://a.org/x.patch), and <https://b.org/y>. Done: https://c.org/z."),
        [
            "https://a.org/x.patch",
            "https://b.org/y",
            "https://c.org/z"
        ]
    );
    assert!(urls_in("http://insecure.org/x https://").is_empty());
}

#[test]
fn only_a_fix_of_the_upstream_repository_is_a_candidate() {
    let t = text(
        &[
            &format!("https://github.com/o/r/commit/{SHA}"),
            "https://github.com/other/repo/commit/fedcba9876543210fedcba9876543210fedcba98",
            "https://github.com/o/r/issues/12",
        ],
        &[
            &format!("fixed in https://github.com/o/r/commit/{SHA}.patch"),
            "see https://example.org/advisory and https://github.com/o/r/pull/13.",
        ],
    );
    assert_eq!(
        candidate_fixes(&[t], None, REPO),
        [
            format!("https://github.com/o/r/commit/{SHA}.patch"),
            "https://github.com/o/r/pull/13.patch".to_string(),
        ]
    );
}

#[test]
fn the_description_names_candidates_too() {
    let got = candidate_fixes(
        &[],
        Some(&format!("upstream: https://github.com/O/R/commit/{SHA}")),
        REPO,
    );
    assert_eq!(got.len(), 1);
    assert!(candidate_fixes(&[], Some("no link"), REPO).is_empty());
}

#[test]
fn a_commit_search_is_a_gh_path_and_skips_reverts_and_forks() {
    assert_eq!(
        search_commits_path("o/r", "CVE-2025-1234"),
        "search/commits?q=CVE-2025-1234+repo%3Ao%2Fr"
    );
    assert_eq!(
        search_commits_path("o/r", "bsc#1"),
        "search/commits?q=bsc%231+repo%3Ao%2Fr"
    );
    let json: Value = serde_json::from_str(SEARCH).unwrap();
    assert_eq!(
        parse_commit_search(&json, REPO),
        [format!("https://github.com/o/r/commit/{SHA}.patch")]
    );
    assert!(parse_commit_search(&Value::Null, REPO).is_empty());
}

#[test]
fn a_tag_contains_a_commit_that_is_behind_it() {
    let behind: Value = serde_json::from_str(BEHIND).unwrap();
    let diverged: Value = serde_json::from_str(DIVERGED).unwrap();
    assert!(tag_contains(&behind));
    assert!(tag_contains(&serde_json::json!({"status": "identical"})));
    assert!(!tag_contains(&diverged));
    assert!(!tag_contains(&serde_json::json!({"status": "ahead"})));
    assert_eq!(
        compare_path("o/r", "v1.2.4", SHA),
        format!("repos/o/r/compare/v1.2.4...{SHA}")
    );
}

#[test]
fn a_tag_stands_for_the_version_from_its_first_digit() {
    assert_eq!(tag_version("v1.2.4"), "1.2.4");
    assert_eq!(tag_version("foo-2.0"), "2.0");
    assert_eq!(tag_version("latest"), "latest");
}
