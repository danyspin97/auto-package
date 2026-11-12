use super::*;

/// Hosts whose URLs are treated as source forges worth trying when they turn
/// up as a homepage in Repology. Anything else (project websites, docs) is
/// ignored: there is no generic way to turn those into a source archive.
pub(super) const FORGE_HOSTS: &[&str] = &["github.com", "gitlab.com", "codeberg.org", "git.sr.ht"];

/// Whether `url` names a repository (`host/owner/repo`), not just an owner,
/// organisation or user page (`host/owner`). Both answer HTTP 200, so a
/// reachability check alone cannot tell them apart — and a profile page has
/// no tags, releases or archives to package.
pub fn is_repository_url(url: &str) -> bool {
    ap_ecosystem::forge::parse(url)
        .is_some_and(|f| f.project_path.split('/').filter(|s| !s.is_empty()).count() >= 2)
}

pub(super) fn normalize(name: &str) -> String {
    crate::sources::normalize_name(name)
}

/// Order `(repo name, url)` hits so exact name matches come first, then close
/// ones (`foo-cpp`, `cpp-foo`, `foo` with separators removed). Unrelated
/// repositories are dropped — a search for `cuda` also returns every project
/// with "cuda" in its description, and none of those is "cuda".
///
/// Input order (the API's own star ranking) is preserved within each bucket.
pub fn rank_name_matches(hits: &[(String, String)], pkg: &str) -> Vec<String> {
    let want = normalize(pkg);
    let squashed = want.replace('-', "");
    let mut exact = Vec::new();
    let mut close = Vec::new();
    for (name, url) in hits {
        let n = normalize(name);
        if n == want {
            exact.push(url.clone());
        } else if n == squashed
            || n.ends_with(&format!("-{want}"))
            || n.starts_with(&format!("{want}-"))
        {
            close.push(url.clone());
        }
    }
    exact.extend(close);
    exact
}

/// Exact-name hits only. Used for the secondary forges, where a "close" match
/// is far more often a port or binding in another language (`foo-go`,
/// `foo-rs`) than the project itself.
pub(super) fn exact_only(hits: &[(String, String)], pkg: &str) -> Vec<String> {
    let want = normalize(pkg);
    hits.iter()
        .filter(|(n, _)| normalize(n) == want)
        .map(|(_, u)| u.clone())
        .collect()
}

/// Hits from GitLab's `/api/v4/projects?search=` response.
pub fn parse_gitlab_search(json: &Value, pkg: &str) -> Vec<String> {
    let hits: Vec<(String, String)> = json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                p.get("path")?.as_str()?.to_string(),
                p.get("web_url")?.as_str()?.to_string(),
            ))
        })
        .collect();
    exact_only(&hits, pkg)
}

/// Hits from Codeberg's (Gitea) `/api/v1/repos/search` response.
pub fn parse_codeberg_search(json: &Value, pkg: &str) -> Vec<String> {
    let hits: Vec<(String, String)> = json
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some((
                r.get("name")?.as_str()?.to_string(),
                r.get("html_url")?.as_str()?.to_string(),
            ))
        })
        .collect();
    exact_only(&hits, pkg)
}

/// What the distribution's own packaging records as a project's upstream.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DistroInfo {
    /// Forge repositories named by the spec's `URL:` / `Source:` tags.
    pub forge_repos: Vec<String>,
    /// Every upstream web address the spec records (`URL:`, plus the
    /// directory of each release tarball). Often the only trace of a project
    /// hosted off the forges.
    pub homepages: Vec<String>,
    /// Release directories derived from `Source:` tarball URLs on non-forge
    /// hosts (`…/releases/lzip/%name-%version.tar.gz` → `…/releases/lzip/`).
    /// Candidates, not facts: each still has to list real releases.
    pub index_candidates: Vec<String>,
}

/// Value of a `Tag:` line, ignoring case and any trailing `#/rename`.
pub(super) fn spec_tag(line: &str, tag: &str) -> Option<String> {
    crate::spec::parse_source_line(line, &[tag], str::to_string)
        .map(|source| source.url.trim().to_string())
}

pub(super) fn expand_name(s: &str, name: &str) -> String {
    s.replace("%{name}", name).replace("%name", name)
}

/// Upstream addresses recorded in a spec file's `URL:` and `Source:` tags.
///
/// Only `%{name}` is expanded: any other macro left in a path means the
/// address is not usable as written, so it is dropped rather than guessed.
/// `.sig`/`.asc`/keyring sources are ignored — only gzip tarballs, the one
/// kind the pipeline can extract, say where releases live.
pub fn parse_spec_upstream(spec: &str, name: &str) -> DistroInfo {
    let mut info = DistroInfo::default();
    let add = |list: &mut Vec<String>, u: String| {
        if !list.contains(&u) {
            list.push(u);
        }
    };
    for raw in spec.lines() {
        let line = raw.trim();
        if let Some(v) = spec_tag(line, "url") {
            let url = expand_name(&v, name);
            if !url.starts_with("http") || url.contains('%') {
                continue;
            }
            let url = url.trim_end_matches('/').to_string();
            if let Some(f) = ap_ecosystem::forge::parse(&url)
                && FORGE_HOSTS.contains(&f.host.as_str())
                && is_repository_url(&url)
            {
                add(&mut info.forge_repos, f.canonical_https());
            }
            add(&mut info.homepages, url);
        } else if let Some(v) = spec_tag(line, "source") {
            let src = expand_name(&v, name);
            let path = src.split(['?', '#']).next().unwrap_or("");
            let file = path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            if !path.starts_with("http") || !(file.ends_with(".tar.gz") || file.ends_with(".tgz")) {
                continue;
            }
            // `…/owner/repo/archive/…tar.gz` is a forge archive, not a directory.
            if let Some(f) = ap_ecosystem::forge::parse(path)
                && FORGE_HOSTS.contains(&f.host.as_str())
            {
                if is_repository_url(&f.canonical_https()) {
                    add(&mut info.forge_repos, f.canonical_https());
                }
                continue;
            }
            let dir = &path[..path.rfind('/').unwrap_or(path.len())];
            if dir.contains('%')
                || dir
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .is_empty()
            {
                continue;
            }
            let dir = format!("{}/", dir.trim_end_matches('/'));
            add(&mut info.homepages, dir.clone());
            add(&mut info.index_candidates, dir);
        }
    }
    info
}

/// The openSUSE:Factory spec for `pkg`, parsed for its upstream. Empty when
/// the package is not in Factory or `osc` is unavailable — this is a hint,
/// never a requirement.
pub fn distro_info(pkg: &str) -> DistroInfo {
    let name = normalize(pkg);
    if !crate::osc::is_valid_obs_name(&name) {
        return DistroInfo::default();
    }
    ap_core::diag!("  checking openSUSE:Factory for {name}");
    let spec_file = format!("{name}.spec");
    let Ok(scratch) = crate::proc::private_dir() else {
        return DistroInfo::default();
    };
    let Ok(out) = crate::proc::capture_with_timeout(
        "osc",
        &["cat", "openSUSE:Factory", &name, &spec_file],
        // `osc` looks for a working copy in the directory it starts in.
        scratch,
        crate::proc::Emit::quiet(),
        Duration::from_secs(45),
    ) else {
        return DistroInfo::default();
    };
    if !out.success() {
        return DistroInfo::default();
    }
    parse_spec_upstream(&out.stdout, &name)
}

pub(super) fn get_json(label: &str, url: &str) -> Option<Value> {
    crate::sources::get_json_for_search(url)
        .inspect_err(|e| ap_core::diag!("  {}", search_failure_line(label, e)))
        .ok()
}

/// One short line for a failed forge search: a refusal says to try later; any
/// other error is cut to its first line.
fn search_failure_line(label: &str, err: &str) -> String {
    // `http status: 429` rather than a bare "429": the URL is part of the text
    // and a query such as `foo429` must not read as a refusal.
    if err.contains("http status: 429") || err.contains("rate-limited") {
        return format!("{label} search rate-limited; try again later");
    }
    // Same rule as `repo_search::failure::one_line`: the first non-blank line.
    let first = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error");
    format!("{label} search failed: {first}")
}

pub fn search_gitlab(pkg: &str) -> Vec<String> {
    let name = normalize(pkg);
    ap_core::diag!("  searching GitLab for: {name}");
    get_json(
        "GitLab",
        &format!(
        "https://gitlab.com/api/v4/projects?search={}&order_by=star_count&sort=desc&per_page=10",
        urlencoding::encode(&name)
    ),
    )
    .map(|j| parse_gitlab_search(&j, pkg))
    .unwrap_or_default()
}

pub fn search_codeberg(pkg: &str) -> Vec<String> {
    let name = normalize(pkg);
    ap_core::diag!("  searching Codeberg for: {name}");
    get_json(
        "Codeberg",
        &format!(
            "https://codeberg.org/api/v1/repos/search?q={}&sort=stars&order=desc&limit=10",
            urlencoding::encode(&name)
        ),
    )
    .map(|j| parse_codeberg_search(&j, pkg))
    .unwrap_or_default()
}

/// Candidates from every non-GitHub source, most trustworthy first (the
/// distro spec's own forge upstream, then GitLab, then Codeberg), without
/// duplicates; plus the distro evidence itself.
pub fn search_other_forges(pkg: &str) -> (Vec<String>, DistroInfo) {
    // The three lookups are independent network calls: run them side by side,
    // and keep the order the results are merged in below.
    let (distro, gitlab, codeberg) = std::thread::scope(|s| {
        let distro = s.spawn(|| distro_info(pkg));
        let gitlab = s.spawn(|| search_gitlab(pkg));
        let codeberg = search_codeberg(pkg);
        let distro = distro
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        let gitlab = gitlab
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        (distro, gitlab, codeberg)
    });
    let mut out: Vec<String> = Vec::new();
    for url in distro
        .forge_repos
        .iter()
        .cloned()
        .chain(gitlab)
        .chain(codeberg)
    {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    (out, distro)
}

/// Whether the candidates found by name search are unconfirmed.
///
/// The distro's spec records an upstream, yet none of the candidates is it. That is the shape
/// of a name collision: the real project lives somewhere the searches do not
/// reach (`lzip` is on Savannah, while an unrelated Python binding called
/// `lzip` sits on GitHub), and the first search hit is the wrong project.
pub fn candidates_unconfirmed(candidates: &[String], distro: &DistroInfo) -> bool {
    let trim = |u: &str| u.trim_end_matches('/').to_ascii_lowercase();
    !distro.homepages.is_empty()
        && !candidates.iter().any(|c| {
            distro.forge_repos.iter().any(|r| same_repo(c, r))
                || distro.index_candidates.iter().any(|i| trim(i) == trim(c))
        })
}

pub(super) fn same_repo(a: &str, b: &str) -> bool {
    match (ap_ecosystem::forge::parse(a), ap_ecosystem::forge::parse(b)) {
        (Some(x), Some(y)) => {
            x.host.eq_ignore_ascii_case(&y.host)
                && x.project_path.eq_ignore_ascii_case(&y.project_path)
        }
        _ => false,
    }
}

#[cfg(test)]
mod search_failure_tests {
    use super::search_failure_line;

    #[test]
    fn rate_limit_is_one_short_line() {
        let e = "https://codeberg.org/api/v1/repos/search?q=x&sort=stars: http status: 429 \
                 (rate-limited; this host takes no token, so try again later)";
        assert_eq!(
            search_failure_line("Codeberg", e),
            "Codeberg search rate-limited; try again later"
        );
    }

    #[test]
    fn a_429_inside_the_url_is_not_a_refusal() {
        let e = "https://gitlab.com/api/v4/projects?search=foo429: http status: 500";
        assert_eq!(
            search_failure_line("GitLab", e),
            "GitLab search failed: https://gitlab.com/api/v4/projects?search=foo429: http status: 500"
        );
    }

    #[test]
    fn other_errors_keep_their_first_line() {
        assert_eq!(
            search_failure_line("GitLab", "boom\nsecond"),
            "GitLab search failed: boom"
        );
    }

    #[test]
    fn blank_leading_lines_are_skipped() {
        assert_eq!(
            search_failure_line("GitLab", "\n   \nsecond"),
            "GitLab search failed: second"
        );
    }

    #[test]
    fn empty_error_says_unknown() {
        assert_eq!(
            search_failure_line("Codeberg", ""),
            "Codeberg search failed: unknown error"
        );
    }
}
