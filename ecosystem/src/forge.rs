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

/// Forges whose host names are fixed. A host matches when it equals an entry or
/// is a dot-delimited subdomain of one (`sub.gitlab.com`), never when it merely
/// starts with or contains one (`gitlab.com.attacker.example`).
pub const FIXED_FORGE_HOSTS: &[&str] = &[
    "github.com",
    "gitlab.com",
    "bitbucket.org",
    "codeberg.org",
    "sr.ht",
    "git.sr.ht",
    "salsa.debian.org",
];

/// Known Git-forge host suffixes, for recognising the forge a URL already in a
/// spec or a checkout points at. A match extends to subdomains too, so a
/// self-hosted `gitlab.gnome.org` or `git.example.com` with a `github.com`
/// mirror reads correctly. GitHub Enterprise lives on bespoke hostnames and
/// is not auto-detected — pass its URL through explicitly if it comes up.
///
/// This is recognition, not trust. Any `gitlab.<x>` name matches the prefix
/// entry, so a URL an agent names must be checked with
/// [`is_fixed_forge_host`] (plus the user's own allowlist) instead.
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

/// Whether `host` is one of [`FIXED_FORGE_HOSTS`] or a subdomain of one. This
/// is the trust test: self-hosted `gitlab.<x>` names do not pass it.
pub fn is_fixed_forge_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    FIXED_FORGE_HOSTS
        .iter()
        .any(|k| host == *k || host.strip_suffix(k).is_some_and(|h| h.ends_with('.')))
}

/// Does `host` look like one of the known forges? Recognition only: see
/// [`KNOWN_HOSTS`]. Use [`is_fixed_forge_host`] for a trust decision.
pub fn is_known_host(host: &str) -> bool {
    // Host names are case-insensitive (`GitHub.com` is `github.com`).
    let host = host.to_ascii_lowercase();
    KNOWN_HOSTS.iter().any(|k| {
        if let Some(prefix) = k.strip_suffix('.') {
            // Prefix rule (`gitlab.`): any `gitlab.<something>` host.
            host.starts_with(*k) && host.len() > prefix.len() + 1
        } else {
            host == *k || host.strip_suffix(k).is_some_and(|h| h.ends_with('.'))
        }
    })
}

/// Hosts running GitLab, where the project path may nest subgroups.
fn is_gitlab_style(host: &str) -> bool {
    host == "salsa.debian.org" || host.split('.').any(|label| label == "gitlab")
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
    //   github.com/owner/repo    (bare, known forge hosts only)
    let hostpath = if let Some((_, rest)) = trimmed.split_once("://") {
        // Drop user@ prefix if present (ssh://git@host/...), but only when
        // the '@' sits in the authority — a later '@' (a `pkg@1.2.0` tag in
        // the path) is not userinfo.
        let authority_end = rest.find('/').unwrap_or(rest.len());
        match rest[..authority_end].rfind('@') {
            Some(i) => rest[i + 1..].to_string(),
            None => rest.to_string(),
        }
    } else if let Some((_user, rest)) = trimmed
        .split_once('@')
        .filter(|(user, _)| !user.contains('/'))
    {
        // scp-style: user@host:path  → host/path
        rest.replacen(':', "/", 1)
    } else if let Some((host, _)) = trimmed.split_once('/')
        && is_known_host(host)
    {
        // A bare `github.com/owner/repo` names a known forge, read as https.
        // Any other scheme-less string (a local path) is not a URL.
        trimmed.to_string()
    } else {
        return None;
    };

    // A query string or fragment is not part of the path (`?tab=readme`,
    // `#installation`); left in, it ends up in the repo name.
    let hostpath = hostpath.split(['?', '#']).next().unwrap_or_default();
    let hostpath = hostpath.trim_end_matches('/');
    let mut parts = hostpath.splitn(2, '/');
    // Host names are case-insensitive; the project path keeps its case.
    let host = parts.next()?.to_ascii_lowercase();
    let path = parts.next()?;
    if host.is_empty() || path.is_empty() {
        return None;
    }

    // GitLab uses `/-/` to separate the repo path from sub-paths (tree,
    // blob, issues, …). Everything before `/-/` is the project path.
    // `/api/v4/projects/<id>/...` is GitLab's API, not a project path.
    if is_gitlab_style(&host) && path.starts_with("api/") {
        return None;
    }
    let project_path = if let Some(idx) = path.find("/-/") {
        &path[..idx]
    } else if is_gitlab_style(&host) {
        // Subgroups make any segment a possible group name
        // (`org/team/security/project`), so nothing is trimmed without `/-/`.
        path
    } else {
        // GitHub (and anything shaped like it) uses literal sub-paths
        // directly after the repo. Trim at the first one we recognise.
        const GH_SUBPATHS: &[&str] = &[
            "archive", "releases", "tree", "blob", "commits", "commit", "issues", "pulls", "pull",
            "wiki", "raw", "tags", "branches", "compare", "graphs", "settings", "actions",
            "security",
        ];
        // Match whole path segments only, and never the first two (owner
        // and repo): `owner/actions-toolkit` or `owner/wiki` are repos.
        let mut cut = path.len();
        let mut offset: usize = 0;
        for (i, seg) in path.split('/').enumerate() {
            if i >= 2 && GH_SUBPATHS.contains(&seg) {
                cut = offset.saturating_sub(1);
                break;
            }
            offset += seg.len() + 1;
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
    fn gitlab_api_url_is_not_a_project() {
        assert!(
            parse(
                "https://gitlab.freedesktop.org/api/v4/projects/26558/packages/generic/a/1/a.tar.xz"
            )
            .is_none()
        );
    }

    #[test]
    fn plain_github_https_url() {
        let f = parse("https://github.com/NVIDIA/cuda-python").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.project_path, "NVIDIA/cuda-python");
        assert_eq!(f.repo_name, "cuda-python");
        assert_eq!(f.canonical_https(), "https://github.com/NVIDIA/cuda-python");
    }

    #[test]
    fn gitlab_subgroups_named_like_github_subpaths_are_kept() {
        let f = parse("https://gitlab.com/org/team/security/project").unwrap();
        assert_eq!(f.project_path, "org/team/security/project");
        let f = parse("https://gitlab.com/org/team/actions/project/-/releases").unwrap();
        assert_eq!(f.project_path, "org/team/actions/project");
        // GitHub-style hosts still trim.
        let f = parse("https://github.com/o/r/security/advisories").unwrap();
        assert_eq!(f.project_path, "o/r");
    }

    #[test]
    fn query_and_fragment_stripped() {
        for url in [
            "https://github.com/owner/repo?tab=readme",
            "https://github.com/owner/repo#readme",
            "https://github.com/owner/repo.git?a=b#c",
            "https://github.com/owner/repo/?tab=readme",
        ] {
            let f = parse(url).unwrap();
            assert_eq!(f.project_path, "owner/repo", "{url}");
            assert_eq!(f.repo_name, "repo", "{url}");
        }
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
    fn lookalike_hosts_are_not_known() {
        assert!(is_known_host("github.com"));
        assert!(is_known_host("www.github.com"));
        assert!(!is_known_host("github.com.evil.net"));
        assert!(!is_known_host("notgitlab.com"));
    }

    #[test]
    fn fixed_forge_hosts_are_trusted_by_domain_only() {
        for host in ["github.com", "gitlab.com", "sub.gitlab.com", "GitLab.com"] {
            assert!(is_fixed_forge_host(host), "{host}");
        }
        for host in [
            "gitlab.com.attacker.example",
            "evil-gitlab.com",
            "notgitlab.com",
            "gitlab.example.org",
            "gitlab.gnome.org",
            "gitlab.",
            "",
        ] {
            assert!(!is_fixed_forge_host(host), "{host}");
        }
    }

    #[test]
    fn self_hosted_gitlab_is_recognised_but_not_trusted() {
        // Recognition (spec URLs) keeps the prefix rule; trust does not.
        assert!(is_known_host("gitlab.example.org"));
        assert!(!is_fixed_forge_host("gitlab.example.org"));
    }

    #[test]
    fn at_sign_in_path_is_not_userinfo() {
        let f = parse("https://github.com/o/r/releases/tag/pkg@1.2.0").unwrap();
        assert_eq!(f.project_path, "o/r");
        let f = parse("https://user@github.com/o/r").unwrap();
        assert_eq!(f.host, "github.com");
    }

    #[test]
    fn sub_paths_match_whole_segments_only() {
        for name in [
            "actions-toolkit",
            "tags-x",
            "wiki-js",
            "security-tool",
            "wiki",
        ] {
            let f = parse(&format!("https://github.com/owner/{name}")).unwrap();
            assert_eq!(f.project_path, format!("owner/{name}"));
        }
        let f = parse("https://github.com/owner/wiki-js/issues").unwrap();
        assert_eq!(f.project_path, "owner/wiki-js");
    }

    #[test]
    fn empty_or_bad_input_returns_none() {
        assert!(parse("").is_none());
        assert!(parse("not-a-url").is_none());
        assert!(parse("https://github.com/just-an-owner").is_none());
        assert!(parse("https://github.com/").is_none());
    }

    #[test]
    fn host_is_lowercased_and_the_project_path_keeps_its_case() {
        let f = parse("GitHub.com/owner/Repo").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.project_path, "owner/Repo");
        assert_eq!(f.repo_name, "Repo");
        let f = parse("https://GitHub.com/owner/Repo").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.canonical_https(), "https://github.com/owner/Repo");
        // Case does not stop the GitLab subgroup rule from applying.
        let f = parse("git@GitLab.COM:Org/Team/Project.git").unwrap();
        assert_eq!(f.host, "gitlab.com");
        assert_eq!(f.project_path, "Org/Team/Project");
    }

    #[test]
    fn bare_known_host_is_read_as_a_forge_url_but_local_paths_are_not() {
        let f = parse("GitHub.com/owner/Repo").unwrap();
        assert_eq!(f.host, "github.com");
        assert_eq!(f.project_path, "owner/Repo");
        assert_eq!(f.repo_name, "Repo");
        assert_eq!(
            parse("gitlab.gnome.org/GNOME/glib").unwrap().host,
            "gitlab.gnome.org"
        );
        // Unknown hosts and local paths without a scheme are still refused.
        assert!(parse("example.com/owner/repo").is_none());
        assert!(parse("some.dir/sub/path").is_none());
        assert!(parse("owner/repo").is_none());
    }

    #[test]
    fn known_host_check_ignores_case() {
        assert!(is_known_host("GitHub.com"));
        assert!(is_known_host("WWW.GitHub.COM"));
        assert!(is_known_host("GitLab.Gnome.org"));
        assert!(!is_known_host("GitHub.com.Evil.net"));
        assert!(is_fixed_forge_host("GitHub.com"));
    }

    #[test]
    fn case_is_preserved_in_the_owner() {
        // `NVIDIA` matters for upstream URLs — don't lowercase the project
        // path, even though an RPM name derived from it would be lowercase.
        let f = parse("https://github.com/NVIDIA/cuda-python").unwrap();
        assert_eq!(f.project_path, "NVIDIA/cuda-python");
    }
}
