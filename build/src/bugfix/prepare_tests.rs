use std::cell::RefCell;

use super::*;
use crate::allowed_hosts::AllowedHosts;
use crate::backport::FixSource;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const COMMIT: &str = include_str!("../testdata/bugfix/github_commit.patch");
const SEARCH: &str = include_str!("../testdata/bugfix/gh_search_commits.json");
const BEHIND: &str = include_str!("../testdata/bugfix/gh_compare_behind.json");
const DIVERGED: &str = include_str!("../testdata/bugfix/gh_compare_diverged.json");
const REPO: &str = "https://github.com/o/r";

#[derive(Default)]
struct Fake {
    patch: Option<String>,
    search: Option<String>,
    compare: Option<String>,
    tags: Vec<String>,
    fetched: RefCell<Vec<String>>,
    json_paths: RefCell<Vec<String>>,
}

impl FixNet for Fake {
    fn get_text(&self, url: &str) -> Result<String, GetError> {
        panic!("no tracker is allowed here: {url}")
    }
    fn fetch_patch(&self, source: &FixSource) -> Result<Vec<u8>, String> {
        self.fetched.borrow_mut().push(source.describe());
        self.patch
            .clone()
            .map(String::into_bytes)
            .ok_or_else(|| "no patch".to_string())
    }
    fn github_json(&self, path: &str) -> Result<serde_json::Value, String> {
        self.json_paths.borrow_mut().push(path.to_string());
        let body = if path.starts_with("search/") {
            self.search.clone()
        } else {
            self.compare.clone()
        };
        serde_json::from_str(&body.ok_or("nothing")?).map_err(|e| e.to_string())
    }
    fn release_tags(&self, _: &str) -> Result<Vec<String>, String> {
        Ok(self.tags.clone())
    }
}

fn input(bugs: &[&str], description: Option<&str>) -> BugInput {
    parse_bug_input(
        &bugs.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
        &[],
        description,
        None,
    )
    .unwrap()
}

fn run(
    net: &Fake,
    input: &BugInput,
    commit_urls: &[String],
    repo: Option<&str>,
) -> Result<FixJob, String> {
    prepare_fix(
        &FixRequest {
            pkg: "foo",
            input,
            commit_urls,
            commit_repo: None,
            upstream_repo: repo.map(str::to_string),
            declared_patches: &[],
            version: "1.2.3",
        },
        &AllowedHosts::empty(),
        net,
    )
}

#[test]
fn a_given_commit_is_fetched_named_and_headed() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        ..Default::default()
    };
    let urls = [format!("https://github.com/o/r/commit/{SHA}")];
    let job = run(
        &net,
        &input(&["CVE-2025-1234", "bsc#1234567"], None),
        &urls,
        Some(REPO),
    )
    .unwrap();
    assert_eq!(job.patches.len(), 1);
    let p = &job.patches[0];
    assert_eq!(p.file_name, "foo-CVE-2025-1234.patch");
    assert_eq!(p.commit.as_deref(), Some(SHA));
    let text = String::from_utf8(p.bytes.clone()).unwrap();
    assert!(
        text.contains("References: CVE-2025-1234 bsc#1234567\n"),
        "{text}"
    );
    assert!(text.contains(&format!(
        "Upstream: merged https://github.com/o/r/commit/{SHA}\n"
    )));
    assert_eq!(job.fetch_hosts, ["github.com"]);
    assert!(job.candidates.is_empty());
    assert!(!job.nonce.is_empty());
}

#[test]
fn a_fix_named_in_the_bug_is_the_one_staged() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        ..Default::default()
    };
    let i = input(
        &["bsc#1234567"],
        Some(&format!(
            "fixed upstream by https://github.com/o/r/commit/{SHA}"
        )),
    );
    let job = run(&net, &i, &[], Some(REPO)).unwrap();
    assert_eq!(job.patches.len(), 1);
    assert_eq!(job.patches[0].file_name, "foo-fix-0123456.patch");
    // The search is not needed when the bug names the fix.
    assert!(
        net.json_paths
            .borrow()
            .iter()
            .all(|p| !p.starts_with("search/"))
    );
}

#[test]
fn two_fixes_are_listed_and_none_is_staged() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        ..Default::default()
    };
    let i = input(
        &["bsc#1234567"],
        Some(&format!(
            "https://github.com/o/r/commit/{SHA} and https://github.com/o/r/pull/13"
        )),
    );
    let job = run(&net, &i, &[], Some(REPO)).unwrap();
    assert!(job.patches.is_empty());
    assert_eq!(job.candidates.len(), 2);
    assert!(net.fetched.borrow().is_empty());
}

#[test]
fn the_commit_search_finds_one_fix_when_the_bug_names_none() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        search: Some(SEARCH.to_string()),
        ..Default::default()
    };
    let job = run(&net, &input(&["CVE-2025-1234"], None), &[], Some(REPO)).unwrap();
    assert_eq!(job.patches.len(), 1);
    assert_eq!(
        net.json_paths.borrow()[0],
        "search/commits?q=CVE-2025-1234+repo%3Ao%2Fr"
    );
}

#[test]
fn nothing_found_leaves_the_fix_to_the_agent() {
    let net = Fake::default();
    let job = run(&net, &input(&[], Some("it crashes")), &[], None).unwrap();
    assert!(job.patches.is_empty() && job.candidates.is_empty());
    assert!(matches!(job.fixed_in, FixedIn::NotChecked(_)));
    assert!(job.fetch_hosts.is_empty());
}

#[test]
fn a_commit_url_that_is_not_a_patch_is_an_error() {
    let net = Fake::default();
    let urls = [format!("https://github.com/o/r/commit/{SHA}")];
    assert!(run(&net, &input(&["bsc#1234567"], None), &urls, Some(REPO)).is_err());
    let bad = ["https://example.org/page.html".to_string()];
    assert!(run(&net, &input(&["bsc#1234567"], None), &bad, Some(REPO)).is_err());
}

#[test]
fn a_bug_url_on_a_host_that_is_not_allowed_is_not_fetched() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        ..Default::default()
    };
    let repo = "https://gitlab.example.org/g/r";
    let i = input(
        &["bsc#1234567"],
        Some(&format!("https://gitlab.example.org/g/r/-/commit/{SHA}")),
    );
    let job = run(&net, &i, &[], Some(repo)).unwrap();
    assert!(job.patches.is_empty());
    assert!(job.not_read.iter().any(|n| n.contains("not allowed")));
}

#[test]
fn the_first_newer_tag_that_has_the_commit_is_the_fixed_release() {
    let urls = [format!("https://github.com/o/r/commit/{SHA}")];
    let mut net = Fake {
        patch: Some(COMMIT.to_string()),
        compare: Some(BEHIND.to_string()),
        tags: ["v1.3.0", "v1.2.4", "v1.2.3", "v1.2.2"]
            .map(String::from)
            .to_vec(),
        ..Default::default()
    };
    let job = run(&net, &input(&["CVE-2025-1234"], None), &urls, Some(REPO)).unwrap();
    // Oldest newer tag first, and only newer ones are compared.
    assert_eq!(job.fixed_in, FixedIn::Tag("v1.2.4".to_string()));
    assert_eq!(
        net.json_paths.borrow()[0],
        format!("repos/o/r/compare/v1.2.4...{SHA}")
    );
    net.compare = Some(DIVERGED.to_string());
    net.json_paths.borrow_mut().clear();
    let job = run(&net, &input(&["CVE-2025-1234"], None), &urls, Some(REPO)).unwrap();
    assert_eq!(job.fixed_in, FixedIn::NoneYet);
}

#[test]
fn the_repository_comes_from_the_spec() {
    let spec = "Name: foo\nVersion: 1.2.3\nUrl: https://github.com/o/r\nSource0: %{url}/archive/v%{version}.tar.gz\n";
    assert_eq!(upstream_repo_of_spec(spec).as_deref(), Some(REPO));
    assert_eq!(
        upstream_repo_of_spec("Name: foo\nVersion: 1\nUrl: https://example.org/foo\n"),
        None
    );
}

#[test]
fn a_subject_with_a_macro_cannot_reach_the_spec_comment_or_the_bullet() {
    let evil = COMMIT.replace("parser: check the length", "x %(touch /tmp/x)");
    let net = Fake {
        patch: Some(evil),
        ..Default::default()
    };
    let urls = [format!("https://github.com/o/r/commit/{SHA}")];
    let job = run(&net, &input(&["CVE-2025-1234"], None), &urls, Some(REPO)).unwrap();
    let p = &job.patches[0];
    assert!(p.reason.contains("%%(touch /tmp/x)"), "{}", p.reason);
    let comment = crate::backport::spec_comment(p, &job.refs);
    let bullet = crate::backport::changes_bullet(p, &job.refs);
    for text in [comment, bullet] {
        assert!(!text.replace("%%", "").contains('%'), "{text}");
    }
}

#[test]
fn the_job_says_how_the_fix_was_found() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        search: Some(SEARCH.to_string()),
        ..Default::default()
    };
    let given = [format!("https://github.com/o/r/commit/{SHA}")];
    let job = run(&net, &input(&["CVE-2025-1234"], None), &given, Some(REPO)).unwrap();
    assert_eq!(job.found, Found::Given);
    let named = input(
        &["CVE-2025-1234"],
        Some(&format!("https://github.com/o/r/commit/{SHA}")),
    );
    assert_eq!(
        run(&net, &named, &[], Some(REPO)).unwrap().found,
        Found::NamedInBug
    );
    let job = run(&net, &input(&["CVE-2025-1234"], None), &[], Some(REPO)).unwrap();
    assert_eq!(job.found, Found::Search);
    let job = run(&Fake::default(), &input(&[], Some("x")), &[], None).unwrap();
    assert_eq!(job.found, Found::NotFound);
}

#[test]
fn a_request_or_an_unvouched_commit_is_unmerged() {
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        ..Default::default()
    };
    // The user's own commit is vouched for; a pull request never is.
    let given = [format!("https://github.com/o/r/commit/{SHA}")];
    let job = run(&net, &input(&["CVE-2025-1234"], None), &given, Some(REPO)).unwrap();
    assert!(job.unmerged.is_empty());
    let pr = ["https://github.com/o/r/pull/13".to_string()];
    let job = run(&net, &input(&["CVE-2025-1234"], None), &pr, Some(REPO)).unwrap();
    assert_eq!(job.unmerged.len(), 1);
    // A commit a bug text names is unmerged until a release contains it.
    let named = input(
        &["CVE-2025-1234"],
        Some(&format!("https://github.com/o/r/commit/{SHA}")),
    );
    let job = run(&net, &named, &[], Some(REPO)).unwrap();
    assert_eq!(job.unmerged.len(), 1);
    let net = Fake {
        patch: Some(COMMIT.to_string()),
        compare: Some(BEHIND.to_string()),
        tags: vec!["v1.2.4".to_string()],
        ..Default::default()
    };
    let job = run(&net, &named, &[], Some(REPO)).unwrap();
    assert!(job.unmerged.is_empty(), "a tag contains it");
}
