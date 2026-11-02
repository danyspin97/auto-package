use super::*;

/// The gzip tarballs in an index for the project `hint` (the index URL's own
/// directory name), as `(file, version)`, newest first with pre-releases
/// last. If nothing in the index carries the hinted name, every tarball is
/// kept — a project may publish under a slightly different name than its
/// directory.
pub fn index_archives(links: &[String], hint: &str) -> Vec<(String, String)> {
    let norm = |s: &str| s.replace('_', "-").to_ascii_lowercase();
    let all: Vec<(String, String, String)> = links
        .iter()
        .filter(|f| is_gzip_archive(f))
        .filter_map(|f| archive_name_version(f).map(|(n, v)| (f.clone(), n, v)))
        .filter_map(|(f, n, v)| release_version(&v).map(|v| (f, n, v)))
        .collect();
    let named: Vec<_> = all
        .iter()
        .filter(|(_, n, _)| norm(n) == norm(hint))
        .cloned()
        .collect();
    let mut chosen = if named.is_empty() { all } else { named };
    chosen.sort_by(|a, b| {
        is_prerelease(&a.2)
            .cmp(&is_prerelease(&b.2))
            .then_with(|| version_cmp(&b.2, &a.2))
    });
    chosen.into_iter().map(|(f, _, v)| (f, v)).collect()
}

/// Whether an index lists gzip tarballs at all, and whether it lists only
/// archive kinds the pipeline cannot extract — used for an accurate error.
pub fn index_has_only_unsupported(links: &[String]) -> bool {
    !links.iter().any(|f| is_gzip_archive(f)) && links.iter().any(|f| is_any_archive(f))
}

pub(crate) fn fetch_index(index_url: &str) -> Result<Vec<String>, String> {
    let body = crate::http::read_text(
        ap_ecosystem::net::call_with_retries(|| {
            crate::http::get(index_url).header("User-Agent", "auto-package")
        })
        .map_err(|e| format!("{index_url}: {e}"))?,
    )
    .map_err(|e| format!("{index_url}: {e}"))?;
    Ok(parse_index_links(&body))
}

pub(crate) fn index_dir(index_url: &str) -> String {
    format!(
        "{}/",
        index_url
            .split(['?', '#'])
            .next()
            .unwrap_or(index_url)
            .trim_end_matches('/')
    )
}

pub(super) fn latest_release_non_forge(url: &str, kind: SourceKind) -> Result<String, String> {
    if kind == SourceKind::DirectArchive {
        return archive_name_version(last_segment(url))
            .map(|(_, v)| v)
            .ok_or_else(|| format!("cannot read a version from the file name in {url}"));
    }
    let links = fetch_index(&index_dir(url))?;
    index_archives(&links, last_segment(url))
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or_else(|| {
            if index_has_only_unsupported(&links) {
                format!(
                    "{url} lists only non-gzip tarballs (.tar.xz/.tar.bz2/...), which \
                     auto-package cannot extract yet; pass --version and a .tar.gz URL"
                )
            } else {
                format!("{url} lists no release tarballs")
            }
        })
}

/// Version equality that ignores a leading `v` and trailing `.0` components,
/// so `1.0` matches `1.0.0` and `v1.0` matches `1.0`.
pub(super) fn same_version(a: &str, b: &str) -> bool {
    fn canon(v: &str) -> &str {
        let mut v = v.strip_prefix(['v', 'V']).unwrap_or(v);
        while let Some(rest) = v.strip_suffix(".0") {
            v = rest;
        }
        v
    }
    canon(a) == canon(b)
}

/// Whether the archive name `file` carries `version` as a whole token, for a
/// name [`archive_name_version`] cannot split (`v1.0.tar.gz`, `foo.tar.gz`).
/// The version must start the stem or follow a `-`/`_` (a `v` may come
/// before it, as in `v1.0`). After it the name must end, or continue with a
/// wheel tag (`-py3-none-any`): the same boundary [`archive_name_version`]
/// gives a parsed name, so `1.0` is not in `v1.0.5.tar.gz`, `x1.0.tar.gz` or
/// `v1.0-docs.tar.gz`.
pub(super) fn file_names_version(file: &str, version: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let Some(ext) = GZIP_EXTS
        .iter()
        .chain(OTHER_ARCHIVE_EXTS)
        .chain(&[".tar", ".whl"])
        .find(|e| lower.ends_with(**e))
    else {
        return false;
    };
    let stem = &lower[..lower.len() - ext.len()];
    let want = version.trim_start_matches(['v', 'V']).to_ascii_lowercase();
    !want.is_empty()
        && stem.match_indices(want.as_str()).any(|(at, _)| {
            let before = &stem[..at];
            let before = before.strip_suffix('v').unwrap_or(before);
            let after = &stem[at + want.len()..];
            (before.is_empty() || before.ends_with(['-', '_']))
                && (after.is_empty() || wheel_tags_follow(after))
        })
}

/// Whether `after` is a wheel's tag part after a version, `-<python>-<abi>-<platform>`
/// as in `-py3-none-any`, optionally preceded by a digits-only build tag
/// (`-1-py3-none-any`). The python tag is `py`, `cp`, `pp`, `ip` or `jy` with a
/// version number (`py3`, `cp311`), or several of them joined by dots
/// (`py2.py3`). Any other dash-separated words (`-docs-html-pdf`) are not one.
pub(super) fn wheel_tags_follow(after: &str) -> bool {
    let Some(rest) = after.strip_prefix('-') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let tags: &[&str] = match parts.as_slice() {
        [build, tags @ ..]
            if tags.len() == 3
                && !build.is_empty()
                && build.chars().all(|c| c.is_ascii_digit()) =>
        {
            tags
        }
        [_, _, _] => &parts,
        _ => return false,
    };
    let [python, abi, platform] = tags else {
        return false;
    };
    is_wheel_python_tag(python) && is_wheel_word(abi) && is_wheel_word(platform)
}

/// `py3`, `cp311`, `pp310`, or dot-joined compounds such as `py2.py3`.
pub(super) fn is_wheel_python_tag(tag: &str) -> bool {
    tag.split('.').all(|part| {
        ["py", "cp", "pp", "ip", "jy"].iter().any(|prefix| {
            part.strip_prefix(prefix)
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
    })
}

/// An ABI or platform tag: letters, digits, `_` and `.` (`none`, `abi3`,
/// `manylinux_2_17_x86_64.manylinux2014_x86_64`).
pub(super) fn is_wheel_word(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

pub(super) fn archive_urls_non_forge(url: &str, kind: SourceKind, version: &str) -> Vec<String> {
    if kind == SourceKind::DirectArchive {
        // A fixed URL cannot serve another version: packaging 3.1.4 as 3.2.0
        // would be silently wrong. The version in the name must be the one
        // requested; a name with no parsable version must still name it.
        let file = last_segment(url);
        match archive_name_version(file) {
            Some((_, v)) if !same_version(&v, version) => {
                ap_core::diag!(
                    "  {url} is version {v}, not the requested {version}; \
                     a direct archive URL cannot provide another version"
                );
                return Vec::new();
            }
            Some(_) => {}
            None if !file_names_version(file, version) => {
                ap_core::diag!(
                    "  {url} names no version that matches the requested {version}; \
                     a direct archive URL cannot be trusted to provide it"
                );
                return Vec::new();
            }
            None => {}
        }
        return vec![url.to_string()];
    }
    let dir = index_dir(url);
    match fetch_index(&dir) {
        Ok(links) => index_archives(&links, last_segment(url))
            .into_iter()
            .filter(|(_, v)| same_version(v, version))
            .map(|(f, _)| format!("{dir}{f}"))
            .collect(),
        Err(e) => {
            ap_core::diag!("  could not read release index: {e}");
            Vec::new()
        }
    }
}

/// [`latest_release`] for several repositories, one thread each, results in
/// the same order as `repo_urls`.
pub fn latest_releases(repo_urls: &[String]) -> Vec<Result<String, String>> {
    std::thread::scope(|s| {
        let lookups: Vec<_> = repo_urls
            .iter()
            .map(|url| s.spawn(move || latest_release(url)))
            .collect();
        lookups
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

/// Ask the forge for the newest release tag of `repo_url`, and reduce it to a
/// version.
///
/// A repository has no package index behind it, so "latest" has to come from
/// the forge's own API. Releases are asked for first and plain tags second: a
/// project that tags but never cuts releases (common outside GitHub) still has
/// a newest version, and a project that does both means the release.
pub fn latest_release(repo_url: &str) -> Result<String, String> {
    match source_kind(repo_url) {
        SourceKind::Forge => {}
        kind => return latest_release_non_forge(repo_url, kind),
    }
    let (host, project_path, repo_name) = owner_repo(repo_url)?;
    if is_sourcehut_host(&host) {
        // SourceHut has no releases API without a token: the newest tag is the release.
        let tags = get_repo_tags(repo_url)?;
        return newest_version(&tags, &repo_name)
            .ok_or_else(|| format!("{repo_url} has no tag that is a version"));
    }
    let endpoints = release_endpoints(&host, &project_path);

    let mut last_err = None;
    for url in &endpoints {
        match get_json(url) {
            Ok(v) => match first_tag(&v) {
                Some(tag) => {
                    if let Some(answered) = answered_repo_name(&v)
                        && !answered.eq_ignore_ascii_case(&repo_name)
                    {
                        return Err(format!(
                            "{repo_url} now belongs to the repository {answered}, so its \
                             releases are of another project; point the package at {answered} \
                             to update it"
                        ));
                    }
                    let version = version_from_tag(&tag, &repo_name);
                    // A tag such as `start` or `latest` marks no release; the
                    // tag list may still have one.
                    if version.chars().any(|c| c.is_ascii_digit()) {
                        return Ok(version);
                    }
                    last_err = Some(format!("{url}: the newest tag {tag:?} is not a version"));
                }
                None => last_err = Some(format!("{url}: no release or tag in the answer")),
            },
            Err(e) => last_err = Some(e),
        }
    }

    Err(format!(
        "could not work out the latest version of {repo_url}; pass --version. \
         Last attempt: {}",
        last_err.unwrap_or_else(|| "no endpoint answered".into())
    ))
}

/// `github.com` or one of its subdomains, matched on the parsed host: a
/// lookalike such as `mygithub.com` is not a GitHub host.
pub(super) fn is_github_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "github.com" || host.ends_with(".github.com")
}

/// A GitLab host: some dot-separated label is `gitlab` or starts with it, such
/// as `gitlab.com`, `gitlab.gnome.org`, `gitlab-internal.corp.example` or
/// `git.gitlab.corp.example`. A label that merely ends with it
/// (`evil-gitlab.example`) is not.
pub(super) fn is_gitlab_host(host: &str) -> bool {
    host.to_ascii_lowercase()
        .split('.')
        .any(|label| label.starts_with("gitlab"))
}

/// Whether the URL `base` is on a GitLab host (see [`is_gitlab_host`]), which
/// decides the archive layout. The API lookup and the archive layout must agree
/// on this, or a self-hosted GitLab gets GitHub-style URLs.
pub(super) fn url_is_gitlab(base: &str) -> bool {
    crate::http::host_of(base).is_some_and(|h| is_gitlab_host(&h))
}

/// The release endpoint and the tag-list fallback for a forge project, in the
/// order they are asked. `project_path` carries the full path below the host,
/// so a nested GitLab group (`group/subgroup/project`) is formatted correctly.
pub(super) fn release_endpoints(host: &str, project_path: &str) -> [String; 2] {
    if is_github_host(host) {
        // GitHub has no subgroups, so project_path is always `owner/repo`.
        [
            format!("https://api.github.com/repos/{project_path}/releases/latest"),
            format!("https://api.github.com/repos/{project_path}/tags?per_page=1"),
        ]
    } else if is_gitlab_host(host) {
        // GitLab identifies projects by the URL-encoded full path.
        let encoded = project_path.replace('/', "%2F");
        [
            format!("https://{host}/api/v4/projects/{encoded}/releases?per_page=1"),
            format!("https://{host}/api/v4/projects/{encoded}/repository/tags?per_page=1"),
        ]
    } else {
        // Gitea and Forgejo, which is what most self-hosted forges are.
        // Neither supports subgroups, so project_path is `owner/repo`.
        [
            format!("https://{host}/api/v1/repos/{project_path}/releases/latest"),
            format!("https://{host}/api/v1/repos/{project_path}/tags?limit=1"),
        ]
    }
}

/// The version inside a release tag: `v1.2.3` → `1.2.3`, `thing-v1.2.3` →
/// `1.2.3`.
///
/// The inverse of the spellings [`archive_urls`] tries, so a resolved version
/// round-trips back to the tag it came from.
pub fn version_from_tag(tag: &str, repo: &str) -> String {
    let mut v = tag.trim();
    for prefix in [
        format!("{repo}-"),
        format!("{repo}_"),
        "release-".to_string(),
        "release/".to_string(),
    ] {
        // `get` rather than slicing: the cut may fall inside a multi-byte char.
        if v.len() > prefix.len()
            && let Some(head) = v.get(..prefix.len())
            && head.eq_ignore_ascii_case(&prefix)
        {
            v = &v[prefix.len()..];
            break;
        }
    }
    // A leading `v` is a tag convention, not part of the version — but only
    // when a digit follows, so a tag like `voyager` is left alone.
    if let Some(rest) = v.strip_prefix(['v', 'V'])
        && rest.starts_with(|c: char| c.is_ascii_digit())
    {
        v = rest;
    }
    v.to_string()
}

/// The tag-listing endpoint of a forge project, chosen the same way as
/// [`release_endpoints`].
pub(super) fn tag_list_url(host: &str, project_path: &str) -> String {
    if is_github_host(host) {
        format!("https://api.github.com/repos/{project_path}/tags?per_page=100")
    } else if is_gitlab_host(host) {
        let encoded = project_path.replace('/', "%2F");
        format!("https://{host}/api/v4/projects/{encoded}/repository/tags?per_page=100")
    } else {
        // Gitea/Forgejo
        format!("https://{host}/api/v1/repos/{project_path}/tags?limit=100")
    }
}

/// Get all tags from a repository via the forge API.
pub(super) fn get_repo_tags(repo_url: &str) -> Result<Vec<String>, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Ok(Vec::new());
    }
    let (host, project_path, _repo_name) = owner_repo(repo_url)?;
    if is_sourcehut_host(&host) {
        return ls_remote_tags(&format!("https://{host}/{project_path}"));
    }

    let json = get_json(&tag_list_url(&host, &project_path))?;

    let tags = match json.as_array() {
        Some(arr) => arr
            .iter()
            .filter_map(|item| {
                item.get("name")
                    .or_else(|| item.get("tag_name"))
                    .and_then(|t| t.as_str())
                    .map(str::to_string)
            })
            .collect(),
        None => vec![],
    };

    Ok(tags)
}

/// Find the best matching tag for a given version.
///
/// First tries an exact match, then a tag that maps to the version (a dashed
/// tag is packaged under the RPM version it maps to: `v2.9.0-1` is `2.9.0.1`),
/// then tries common patterns, then returns None. Of the tags that map to the
/// version, the smallest name is taken, whatever order the forge lists them in.
pub(super) fn find_matching_tag(tags: &[String], version: &str, repo: &str) -> Option<String> {
    if let Some(tag) = tags
        .iter()
        .find(|tag| version_from_tag(tag, repo) == version)
    {
        return Some(tag.clone());
    }
    let mapped = tags
        .iter()
        .filter(|tag| crate::upstream::rpm_version_for_tag(tag).as_deref() == Ok(version))
        .min();
    if let Some(tag) = mapped {
        return Some(tag.clone());
    }

    // Try pattern matching (v-prefix, repo-prefix, etc.)
    let patterns = [
        format!("v{}", version),
        version.to_string(),
        format!("{}-{}", repo, version),
        format!("{}-v{}", repo, version),
        format!("release-{}", version),
        format!("release/{}", version),
    ];

    for pattern in &patterns {
        if tags.iter().any(|tag| tag == pattern) {
            return Some(pattern.clone());
        }
    }

    None
}

/// Candidate archive URLs for a release tag, best first.
///
/// Queries the forge API for actual tags and tries to find an exact match
/// for `version`. If no exact match is found in the first 100 tags the API
/// returns, this used to silently pick the latest available tag and let
/// the caller name the resulting tarball with the REQUESTED version — the
/// recipe then packaged completely different source bytes under the
/// requested version. The Sep review reproduced this and the Oct follow-up
/// confirmed it was still live: fetching e.g. `demo==1.5.0` against a repo
/// whose first 100 tags only cover `1.6.x+` happily built `1.6.3` and
/// labelled the result `1.5.0`.
///
/// The identity-breaking fallback is removed. If the exact version is not
/// present among the API tags, we return pattern-based guesses only
/// (`v<ver>`, `<ver>`, `<repo>-<ver>`, …) — the HTTP fetch then 404s on
/// every guess and the caller fails loudly, surfacing "this release is
/// not where you think it is" instead of silently substituting a
/// different one.
///
/// Forges disagree on the path: GitHub and the Gitea/Forgejo family serve
/// `/archive/refs/tags/<tag>.tar.gz`, GitLab serves
/// `/-/archive/<tag>/<repo>-<tag>.tar.gz`.
pub fn archive_urls(repo_url: &str, version: &str) -> Vec<String> {
    match source_kind(repo_url) {
        SourceKind::Forge => {}
        kind => return archive_urls_non_forge(repo_url, kind, version),
    }
    // Build from the PARSED repository (host/owner/repo), not the raw string:
    // `…/tree/main`, `git@host:o/r`, `?query` and `.git` all used to leak
    // into the archive URL and make the repo name `main`.
    let parsed = ap_ecosystem::forge::parse(repo_url);
    let base = parsed
        .as_ref()
        .map(|f| f.canonical_https())
        .unwrap_or_else(|| {
            repo_url
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_string()
        });
    let base = base.as_str();
    let repo = parsed
        .as_ref()
        .map(|f| f.repo_name.as_str())
        .unwrap_or_else(|| base.rsplit('/').next().unwrap_or("source"));
    let is_gitlab = url_is_gitlab(base);

    let mut tags = Vec::new();

    // Query API first for an exact match.
    match get_repo_tags(repo_url) {
        Ok(repo_tags) => {
            ap_core::diag!("  found {} tags via API", repo_tags.len());
            if let Some(matching_tag) = find_matching_tag(&repo_tags, version, repo) {
                ap_core::diag!("  ✓ found matching tag: {}", matching_tag);
                tags.push(matching_tag);
            } else {
                ap_core::diag!(
                    "  ⚠ version {version} not found in the first {} API tags; \
                     tag-pattern guesses only (the download will 404 if none hit, \
                     rather than silently substituting a different release)",
                    repo_tags.len()
                );
            }
        }
        Err(e) => {
            ap_core::diag!("  API query failed ({e}); using pattern-based tag guessing");
        }
    }

    // Pattern-based guesses: added unconditionally. When the API resolved
    // an exact tag they are redundant (will 404 or match the already-
    // picked one and get deduped); when the API didn't resolve, they are
    // the only chance of a fetch succeeding, and the caller treats a
    // 404-on-every-URL as the hard failure it should be.
    tags.extend([
        format!("v{}", version),
        version.to_string(),
        format!("{}-{}", repo, version),
        format!("{}-v{}", repo, version),
        format!("release-{}", version),
        format!("release/{}", version),
    ]);

    // Remove duplicates while preserving order
    let mut seen = std::collections::HashSet::new();
    let unique_tags: Vec<_> = tags
        .into_iter()
        .filter(|tag| seen.insert(tag.clone()))
        .collect();

    // Convert tags to URLs
    unique_tags
        .iter()
        .map(|tag| tag_archive_url_for(base, repo, is_gitlab, tag))
        .collect()
}

/// The archive URL of one tag. A tag from the API or a guess may hold
/// characters that are not URL-safe; each segment is encoded but `/` is kept
/// (`release/1.0`).
pub(super) fn tag_archive_url_for(base: &str, repo: &str, is_gitlab: bool, tag: &str) -> String {
    let tag = tag
        .split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if is_gitlab {
        format!("{base}/-/archive/{tag}/{repo}-{tag}.tar.gz")
    } else if crate::http::host_of(base).is_some_and(|h| is_sourcehut_host(&h)) {
        format!("{base}/archive/{tag}.tar.gz")
    } else {
        format!("{base}/archive/refs/tags/{tag}.tar.gz")
    }
}

/// The archive URL of the tag the forge API lists for `version`, or `None` when
/// the repository is not on a forge, the API does not answer or no listed tag
/// is that version. Unlike [`archive_urls`] there is no guess: a caller that
/// writes the URL into a spec without downloading it first needs a tag that
/// exists.
/// The tag the forge API lists for `version`.
pub fn tag_for_version(repo_url: &str, version: &str) -> Option<String> {
    let parsed = ap_ecosystem::forge::parse(repo_url)?;
    let tags = get_repo_tags(repo_url).ok()?;
    find_matching_tag(&tags, version, &parsed.repo_name)
}

pub fn tag_archive_url(repo_url: &str, version: &str) -> Option<String> {
    let parsed = ap_ecosystem::forge::parse(repo_url)?;
    let tags = get_repo_tags(repo_url).ok()?;
    let tag = find_matching_tag(&tags, version, &parsed.repo_name)?;
    let base = parsed.canonical_https();
    let is_gitlab = url_is_gitlab(&base);
    Some(tag_archive_url_for(
        &base,
        &parsed.repo_name,
        is_gitlab,
        &tag,
    ))
}
