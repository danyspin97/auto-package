//! Shared Git-forge URL parsing: SSH→HTTPS normalisation, nested-group paths.
//!
//! Three places in the codebase used to each roll their own forge-URL
//! splitter — `ecosystem::pypi::source_url_from_json`,
//! `build::sources::owner_repo`, and `build::update::git_repo_url` — and all
//! three got the same two things wrong:
//!
//! - They capped the project path at two segments (`owner/repo`), so a
//!   GitLab subgroup like `gitlab.com/group/subgroup/project` was truncated
//!   to `group/subgroup`, which doesn't resolve to anything on the GitLab
//!   API and nowhere on the filesystem either.
//! - They accepted the SSH spelling (`git@github.com:owner/repo.git`) but
//!   never normalised it to an HTTPS URL. Code paths that fed the string
//!   back into `ureq::get` or embedded it in a `Source:` line therefore
//!   produced broken URLs.
//!
//! Both are centralised here so the fix lives in one place.

/// A parsed forge URL, broken down into pieces the different callers want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeUrl {
    /// Host part, e.g. `github.com`, `gitlab.com`, `gitlab.freedesktop.org`.
    pub host: String,
    /// Full project path below the host, e.g. `owner/repo` for GitHub or
    /// `group/subgroup/project` for a nested GitLab group.
    pub project_path: String,
    /// Just the last segment of the project path, e.g. `repo` or `project` —
    /// what shows up in archive filenames.
    pub repo_name: String,
}

impl ForgeUrl {
    /// A canonical `https://<host>/<project_path>` URL — the one shape every
    /// caller can safely feed back into an HTTP client or embed in a spec.
    pub fn canonical_https(&self) -> String {
        format!("https://{}/{}", self.host, self.project_path)
    }

    /// URL-encoded project path (slashes → `%2F`), for the GitLab REST API
    /// which identifies projects by their encoded full path.
    pub fn gitlab_encoded_path(&self) -> String {
        self.project_path.replace('/', "%2F")
    }
}

/// Known Git-forge host suffixes. A match extends to subdomains too, so a
/// self-hosted `gitlab.gnome.org` or `git.example.com` with a `github.com`
/// mirror reads correctly. GitHub Enterprise lives on bespoke hostnames and
/// is not auto-detected — pass its URL through explicitly if it comes up.
pub const KNOWN_HOSTS: &[&str] = &[
    "github.com",
    "gitlab.com",
    "gitlab.", // Self-hosted GitLab (gitlab.freedesktop.org, gitlab.gnome.org, …)
    "bitbucket.org",
    "codeberg.org",
    "sr.ht",
    "salsa.debian.org",
    "git.sr.ht",
];

/// Does `host` look like one of the known forges?
pub fn is_known_host(host: &str) -> bool {
    KNOWN_HOSTS.iter().any(|k| host == *k || host.contains(k))
}

/// Parse any forge URL spelling into its parts, dropping sub-paths that
/// aren't part of the project path itself.
///
/// Accepts:
/// - `https://<host>/<owner>/<repo>` (and nested `.../group/subgroup/project`)
/// - `https://<host>/<path>.git`
/// - `git@<host>:<owner>/<repo>.git` (SSH scp-style)
/// - `ssh://git@<host>/<path>`
/// - Any of the above with trailing `/`, a `/-/<...>` GitLab sub-path, or a
///   GitHub-style suffix like `/tree/main`, `/archive/<tag>.tar.gz`,
///   `/issues`, `/releases/...`, `/pull/...`, `/blob/...`, `/commits/...`,
///   `/wiki`, `/tags/...`.
///
/// Returns `None` for inputs that can't be reduced to at least a
/// `host/owner/repo` path. The host is accepted even when it isn't on
/// [`KNOWN_HOSTS`] — the whole URL is still a syntactically valid forge
/// URL; it's up to the caller to check the host when they only want known
/// forges.
pub fn parse(url: &str) -> Option<ForgeUrl> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Peel off the scheme to get `host/path`. Three shapes handled:
    //   https://host/path
    //   ssh://git@host/path
    //   git@host:owner/path.git  (scp-style, no scheme at all)
    let hostpath = if let Some((_, rest)) = trimmed.split_once("://") {
        // Drop user@ prefix if present (ssh://git@host/...).
        rest.split_once('@')
            .map(|(_, r)| r)
            .unwrap_or(rest)
            .to_string()
    } else if let Some((_user, rest)) = trimmed.split_once('@') {
        // scp-style: user@host:path  → host/path
        rest.replacen(':', "/", 1)
    } else {
        return None;
    };

    let hostpath = hostpath.trim_end_matches('/');
    let mut parts = hostpath.splitn(2, '/');
    let host = parts.next()?.to_string();
    let path = parts.next()?;
    if host.is_empty() || path.is_empty() {
        return None;
    }

    // GitLab uses `/-/` to separate the repo path from sub-paths (tree,
    // blob, issues, …). Everything before `/-/` is the project path.
    let project_path = if let Some(idx) = path.find("/-/") {
        &path[..idx]
    } else {
        // GitHub (and anything shaped like it) uses literal sub-paths
        // directly after the repo. Trim at the first one we recognise.
        const GH_SUBPATHS: &[&str] = &[
            "/archive/",
            "/releases/",
            "/releases",
            "/tree/",
            "/blob/",
            "/commits/",
            "/commit/",
            "/issues/",
            "/issues",
            "/pulls/",
            "/pulls",
            "/pull/",
            "/wiki/",
            "/wiki",
            "/raw/",
            "/tags/",
            "/tags",
            "/branches/",
            "/compare/",
            "/graphs/",
            "/settings",
            "/actions",
            "/security",
        ];
        let mut cut = path.len();
        for sp in GH_SUBPATHS {
            if let Some(idx) = path.find(sp) {
                cut = cut.min(idx);
            }
        }
        &path[..cut]
    };

    // Trim trailing `.git` and trailing slashes — a user-pasted URL often has
    // one or the other, and neither is part of the project path.
    let project_path = project_path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_string();

    // A forge project path has at least two segments: owner and repo (or
    // more, for nested groups). Fewer than two means the URL points at a
    // user profile or group root, which isn't a repository.
    let segments: Vec<&str> = project_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }

    let repo_name = (*segments.last()?).to_string();
    Some(ForgeUrl {
        host,
        project_path: segments.join("/"),
        repo_name,
    })
}

/// Convenience: parse `url`, then return its canonical HTTPS form — the
/// `Source:`-safe spelling. `None` if `url` isn't recognisable as a forge URL.
pub fn canonical_https(url: &str) -> Option<String> {
    parse(url).map(|f| f.canonical_https())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_github_https_url() {
        let f = parse("https://github.com/NVIDIA/cuda-python").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.project_path, "NVIDIA/cuda-python");
        assert_eq!(f.repo_name, "cuda-python");
        assert_eq!(f.canonical_https(), "https://github.com/NVIDIA/cuda-python");
    }

    #[test]
    fn trailing_slash_and_dot_git_dropped() {
        for url in [
            "https://github.com/pallets/markupsafe/",
            "https://github.com/pallets/markupsafe.git",
            "https://github.com/pallets/markupsafe.git/",
        ] {
            let f = parse(url).unwrap();
            assert_eq!(f.project_path, "pallets/markupsafe", "{url}");
        }
    }

    #[test]
    fn ssh_scp_style_normalises_to_https() {
        // The actual bug: a `git@github.com:owner/repo.git` spelling used to
        // pass through unchanged. Downstream, it would be plugged into a
        // `Source:` line as-is and the resulting URL was unreachable.
        let f = parse("git@github.com:NVIDIA/cuda-python.git").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.project_path, "NVIDIA/cuda-python");
        assert_eq!(f.canonical_https(), "https://github.com/NVIDIA/cuda-python",);
    }

    #[test]
    fn ssh_url_scheme_normalises_to_https() {
        let f = parse("ssh://git@gitlab.com/group/project.git").unwrap();
        assert_eq!(f.host, "gitlab.com");
        assert_eq!(f.project_path, "group/project");
        assert_eq!(f.canonical_https(), "https://gitlab.com/group/project");
    }

    #[test]
    fn gitlab_nested_group_preserves_full_path() {
        // The second bug: `group/subgroup/project` used to be truncated to
        // `group/subgroup`, which is a group URL — the GitLab API returns
        // 404 for it, and no archive URL built from it exists.
        let f = parse("https://gitlab.com/group/subgroup/project").unwrap();
        assert_eq!(f.project_path, "group/subgroup/project");
        assert_eq!(f.repo_name, "project");
        assert_eq!(
            f.canonical_https(),
            "https://gitlab.com/group/subgroup/project",
        );
        assert_eq!(f.gitlab_encoded_path(), "group%2Fsubgroup%2Fproject");
    }

    #[test]
    fn gitlab_nested_group_with_sub_path() {
        // `/-/tree/main` sits after the repo path and must be dropped before
        // we compute the project path.
        let f = parse("https://gitlab.com/group/subgroup/project/-/tree/main").unwrap();
        assert_eq!(f.project_path, "group/subgroup/project");
    }

    #[test]
    fn github_sub_paths_are_trimmed() {
        for (url, expected) in [
            (
                "https://github.com/pallets/markupsafe/tree/main",
                "pallets/markupsafe",
            ),
            (
                "https://github.com/pallets/markupsafe/issues/1",
                "pallets/markupsafe",
            ),
            (
                "https://github.com/BurntSushi/ripgrep/archive/14.1.0.tar.gz",
                "BurntSushi/ripgrep",
            ),
            (
                "https://github.com/pallets/markupsafe/blob/main/README.md",
                "pallets/markupsafe",
            ),
            ("https://github.com/foo/bar/releases/tag/v1.0", "foo/bar"),
        ] {
            let f = parse(url).unwrap();
            assert_eq!(f.project_path, expected, "{url}");
        }
    }

    #[test]
    fn self_hosted_gitlab_is_still_recognised() {
        assert!(is_known_host("gitlab.freedesktop.org"));
        assert!(is_known_host("gitlab.gnome.org"));
        assert!(!is_known_host("example.com"));
    }

    #[test]
    fn empty_or_bad_input_returns_none() {
        assert!(parse("").is_none());
        assert!(parse("not-a-url").is_none());
        assert!(parse("https://github.com/just-an-owner").is_none());
        assert!(parse("https://github.com/").is_none());
    }

    #[test]
    fn case_is_preserved_in_the_owner() {
        // `NVIDIA` matters for upstream URLs — don't lowercase the project
        // path, even though an RPM name derived from it would be lowercase.
        let f = parse("https://github.com/NVIDIA/cuda-python").unwrap();
        assert_eq!(f.project_path, "NVIDIA/cuda-python");
    }
}
