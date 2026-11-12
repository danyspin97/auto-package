//! The repositories a name search found that nothing vouches for, as a short
//! list a person can choose from: how each was found and what GitHub says
//! about it. [`candidate_list`] is pure; [`fill_metadata`] asks a closure for
//! the JSON, so tests give it fixtures and [`fetch_metadata`] gives it GitHub.

use super::RepoCandidates;

/// The most repositories listed.
pub const MAX_CANDIDATES: usize = 5;

/// How a candidate came to be listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// A hit of the repository name search.
    Search,
    /// The repository the agent fallback picked (also when a search hit it).
    Agent,
    /// A generated guess of the repository's address.
    Guess,
}

/// A repository that may be the package's source, with what is known of it.
/// A field is `None` when it was not asked (not on github.com) or not answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub source: CandidateSource,
    pub stars: Option<u64>,
    pub releases: Option<u64>,
    pub archived: Option<bool>,
    pub fork: Option<bool>,
}

impl Candidate {
    pub fn new(url: &str, source: CandidateSource) -> Self {
        Candidate {
            url: url.to_string(),
            source,
            stars: None,
            releases: None,
            archived: None,
            fork: None,
        }
    }

    /// Whether the repository's name is the package's, by
    /// [`crate::sources::normalize_name`] on the last path segment.
    pub fn is_exact(&self, name: &str) -> bool {
        last_segment(&self.url).is_some_and(|s| {
            crate::sources::normalize_name(&s) == crate::sources::normalize_name(name)
        })
    }
}

/// The last path segment of a URL, without a `.git` suffix.
fn last_segment(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?.trim_end_matches('/');
    let seg = path.rsplit('/').next()?;
    let seg = seg.strip_suffix(".git").unwrap_or(seg);
    (!seg.is_empty() && !seg.contains(':')).then(|| seg.to_string())
}

/// A URL as compared for de-duplication: case, a trailing slash and `.git`
/// ignored.
fn same_key(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

/// The candidates for `name` from a search that verified nothing: exact-name
/// hits first, then close names, then the agent's pick; each URL once, at most
/// [`MAX_CANDIDATES`]. A hit the agent also picked is listed as the agent's.
pub fn candidate_list(found: &RepoCandidates, name: &str) -> Vec<Candidate> {
    let pick = found.agent_pick.as_deref().map(same_key);
    let mut all: Vec<Candidate> = Vec::new();
    let mut add = |url: &str, source: CandidateSource| {
        if all.iter().any(|c| same_key(&c.url) == same_key(url)) {
            return;
        }
        let source = if pick.as_deref() == Some(same_key(url).as_str()) {
            CandidateSource::Agent
        } else {
            source
        };
        all.push(Candidate::new(url, source));
    };
    for url in &found.unvouched_search_hits {
        add(url, CandidateSource::Search);
    }
    if let Some(url) = &found.agent_pick {
        add(url, CandidateSource::Agent);
    }
    // Stable: exact names before close ones, each group in search order, and
    // the agent's pick (appended last) after the hits of its own group.
    all.sort_by_key(|c| {
        (
            !c.is_exact(name),
            c.source == CandidateSource::Agent && !c.is_exact(name),
        )
    });
    all.truncate(MAX_CANDIDATES);
    all
}

/// The GitHub API URLs for a `github.com` repository: its record and its
/// releases. `None` for any other host.
fn github_api_urls(url: &str) -> Option<(String, String)> {
    let f = ap_ecosystem::forge::parse(url).filter(|f| f.host == "github.com")?;
    let repo = format!("https://api.github.com/repos/{}", f.project_path);
    let releases = format!("{repo}/releases?per_page=100");
    Some((repo, releases))
}

/// Fill the metadata of the `github.com` candidates, each from at most two
/// requests (the repository, then its releases) made through `get`, which
/// returns the JSON or `None`. Candidates run in parallel; an unanswered
/// request leaves its fields `None`. Makes no request when none is on GitHub.
pub fn fill_metadata(
    candidates: &mut [Candidate],
    get: &(dyn Fn(&str) -> Option<serde_json::Value> + Sync),
) {
    std::thread::scope(|s| {
        for c in candidates.iter_mut() {
            let Some((repo, releases)) = github_api_urls(&c.url) else {
                continue;
            };
            s.spawn(move || {
                let Some(r) = get(&repo) else { return };
                c.stars = r.get("stargazers_count").and_then(|v| v.as_u64());
                c.archived = r.get("archived").and_then(|v| v.as_bool());
                c.fork = r.get("fork").and_then(|v| v.as_bool());
                c.releases = get(&releases).and_then(|v| v.as_array().map(|a| a.len() as u64));
            });
        }
    });
}

/// [`fill_metadata`] from the GitHub API, through the `gh` CLI when it is
/// installed (see [`crate::sources::get_json_for_search`]).
pub fn fetch_metadata(candidates: &mut [Candidate]) {
    fill_metadata(candidates, &|url| {
        crate::sources::get_json_for_search(url).ok()
    });
}

#[cfg(test)]
mod tests;
