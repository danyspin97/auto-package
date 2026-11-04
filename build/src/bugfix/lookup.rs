//! The upstream fix of a bug: which commit or pull request of the package's
//! own repository fixes it, and which upstream release contains it.

use serde_json::Value;

use crate::backport;
use crate::bugfix::BugText;

/// The https URLs in `text`, in order, without the punctuation that ends a
/// sentence.
pub fn urls_in(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || "<>\"'()[]{}".contains(c))
        .filter(|t| t.starts_with("https://"))
        .map(|t| {
            t.trim_end_matches(['.', ',', ';', ':', '!', '?'])
                .to_string()
        })
        .filter(|t| t.len() > "https://".len())
        .collect()
}

/// Whether `url` is on the same host and project path as `repo`.
pub fn same_project(url: &str, repo: &str) -> bool {
    match (
        ap_ecosystem::forge::parse(url),
        ap_ecosystem::forge::parse(repo),
    ) {
        (Some(a), Some(b)) => {
            a.host == b.host && a.project_path.eq_ignore_ascii_case(&b.project_path)
        }
        _ => false,
    }
}

/// The patch URLs that the bug texts and the description name and that point
/// into `repo`, each fix once (a commit as `.../commit/SHA` and as
/// `.../commit/SHA.patch` is one). A URL from a bug is untrusted: only a
/// commit, a pull request or a patch file of the package's own repository is
/// a candidate.
pub fn candidate_fixes(texts: &[BugText], description: Option<&str>, repo: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for t in texts {
        urls.extend(t.urls.iter().cloned());
        urls.extend(urls_in(&t.summary));
        for c in &t.comments {
            urls.extend(urls_in(c));
        }
    }
    if let Some(d) = description {
        urls.extend(urls_in(d));
    }
    let mut out: Vec<String> = Vec::new();
    for u in urls {
        if !same_project(&u, repo) {
            continue;
        }
        if let Ok(p) = backport::patch_url(&u)
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

/// The `gh api` path of a commit search for `term` in the GitHub project
/// `project_path`.
pub fn search_commits_path(project_path: &str, term: &str) -> String {
    let q = format!("{term} repo:{project_path}");
    format!(
        "search/commits?q={}",
        urlencoding::encode(&q).replace("%20", "+")
    )
}

/// The patch URLs of the commits a search found, without a revert.
pub fn parse_commit_search(json: &Value, repo: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in json
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let message = item
            .pointer("/commit/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message.trim_start().starts_with("Revert") {
            continue;
        }
        let Some(url) = item.get("html_url").and_then(Value::as_str) else {
            continue;
        };
        if same_project(url, repo)
            && let Ok(p) = backport::patch_url(url)
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

/// The `gh api` path that compares a tag with a commit.
pub fn compare_path(project_path: &str, tag: &str, sha: &str) -> String {
    format!("repos/{project_path}/compare/{tag}...{sha}")
}

/// Whether the comparison of `tag...commit` says the tag contains the commit:
/// the commit is behind the tag or is the tag.
pub fn tag_contains(compare: &Value) -> bool {
    matches!(
        compare.get("status").and_then(Value::as_str),
        Some("behind" | "identical")
    )
}

/// The version a release tag stands for: from its first digit on.
pub fn tag_version(tag: &str) -> &str {
    tag.find(|c: char| c.is_ascii_digit())
        .map_or(tag, |i| &tag[i..])
}
