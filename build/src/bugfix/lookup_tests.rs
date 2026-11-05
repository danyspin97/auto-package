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

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|t| t.to_string()).collect()
}

#[test]
fn release_containing_asks_the_packaged_tag_first() {
    let all = tags(&["v1.3", "v1.2", "v1.1"]);
    let mut asked = Vec::new();
    let got = release_containing(&all, "1.2", 4, true, &mut |t: &str| {
        asked.push(t.to_string());
        Ok(t == "v1.2")
    });
    assert_eq!(got, Ok(ReleaseHas::Packaged));
    assert_eq!(asked, ["v1.2"]);
    let got = release_containing(&all, "1.2", 4, true, &mut |t: &str| Ok(t == "v1.3"));
    assert_eq!(got, Ok(ReleaseHas::Newer("v1.3".into())));
    let got = release_containing(&all, "1.2", 4, false, &mut |t: &str| Ok(t == "v1.2"));
    assert_eq!(got, Ok(ReleaseHas::None));
}

#[test]
fn release_containing_takes_the_oldest_newer_and_stops_at_the_limit() {
    let all = tags(&["v1.4", "v1.3", "v1.2"]);
    let got = release_containing(&all, "1.2", 8, false, &mut |_: &str| Ok(true));
    assert_eq!(got, Ok(ReleaseHas::Newer("v1.3".into())));
    let mut asked = Vec::new();
    let got = release_containing(&all, "1.2", 1, false, &mut |t: &str| {
        asked.push(t.to_string());
        Ok(t == "v1.4")
    });
    assert_eq!(got, Ok(ReleaseHas::None));
    assert_eq!(asked, ["v1.3"]);
}

#[test]
fn release_containing_stops_at_the_first_error() {
    let all = tags(&["v1.3", "v1.2"]);
    let got = release_containing(
        &all,
        "1.2",
        4,
        true,
        &mut |_: &str| Err("rate limit".into()),
    );
    assert_eq!(got, Err("rate limit".to_string()));
}
