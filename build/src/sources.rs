//! User-supplied source repositories for packages that have none on PyPI.
//!
//! A wheel-only package whose metadata names no repository cannot be built at
//! all — there is nothing to put in `Source0`. That is not a rare corner:
//! NVIDIA ships most of its CUDA Python packages as wheels with no
//! `project_urls` at all, and the same is true of plenty of vendor SDKs.
//!
//! The answer is to let the user say where the source lives. The mapping is
//! keyed by package rather than passed as a single flag because the package
//! that needs it is usually a *dependency* discovered mid-run, not the one the
//! user named; and it is persisted to `<work-dir>/sources.toml` so the answer
//! is given once rather than on every invocation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Package name (normalized) → source repository URL.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceMap {
    #[serde(default, rename = "source")]
    urls: BTreeMap<String, String>,
}

/// Normalize a PyPI name for lookup: lowercase, `-`/`.` → `_`.
fn key(name: &str) -> String {
    name.to_lowercase().replace(['-', '.'], "_")
}

impl SourceMap {
    pub fn path(work_dir: &Path) -> PathBuf {
        work_dir.join("sources.toml")
    }

    /// Read `<work-dir>/sources.toml`, or an empty map if it is absent.
    ///
    /// A malformed file is an error the user needs to see — it means an
    /// override they wrote is silently not being applied.
    pub fn load(work_dir: &Path) -> Result<Self, String> {
        let path = Self::path(work_dir);
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("reading {}: {e}", path.display())),
            Ok(text) => {
                toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
            }
        }
    }

    pub fn save(&self, work_dir: &Path) -> Result<(), String> {
        let path = Self::path(work_dir);
        let text =
            toml::to_string_pretty(self).map_err(|e| format!("serializing source map: {e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
    }

    pub fn get(&self, pkg: &str) -> Option<&str> {
        self.urls.get(&key(pkg)).map(String::as_str)
    }

    pub fn insert(&mut self, pkg: &str, url: &str) {
        self.urls
            .insert(key(pkg), url.trim().trim_end_matches('/').to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    /// Parse a `--source-url` value: either `<pkg>=<url>` or a bare `<url>`,
    /// in which case it applies to `default_pkg` (the package being built).
    pub fn parse_arg(arg: &str, default_pkg: &str) -> Result<(String, String), String> {
        // Split on the first `=` that is not part of the scheme or a query
        // string: `https://...` has no bare `=` before the first `/`.
        match arg.split_once('=') {
            Some((pkg, url)) if !pkg.contains("://") && !pkg.contains('/') => {
                if url.trim().is_empty() {
                    return Err(format!("--source-url {arg}: no URL after '='"));
                }
                Ok((pkg.trim().to_string(), url.trim().to_string()))
            }
            _ => {
                if !arg.contains("://") {
                    return Err(format!(
                        "--source-url {arg}: expected <package>=<url> or a bare url"
                    ));
                }
                Ok((default_pkg.to_string(), arg.trim().to_string()))
            }
        }
    }
}

/// Which toolchain a source repository is built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    /// A Python project: `%{python_module …}` macros, `python-` RPM prefix.
    Python,
    /// A standalone Rust crate: cargo-packaging macros, unprefixed RPM name.
    Rust,
    /// A plain C/C++ project (CMake or autotools, no Python/Cargo at all):
    /// unprefixed RPM name, same as Rust — see the `plain-c-cpp-packaging`
    /// skill.
    PlainC,
}

impl Ecosystem {
    /// The openSUSE package name for `pkg` built under this ecosystem.
    pub fn rpm_name(self, pkg: &str) -> String {
        match self {
            Ecosystem::Python => format!("python-{}", pkg.to_lowercase()),
            Ecosystem::Rust | Ecosystem::PlainC => pkg.to_lowercase(),
        }
    }
}

/// What a release archive turned out to be.
#[derive(Debug, Clone)]
pub struct RepoProbe {
    pub ecosystem: Ecosystem,
    /// The name the project declares for itself, when it declares one. The
    /// repository's directory name is only a guess at this — `pallets/flask`
    /// ships `Flask`, and plenty of repositories are named nothing like the
    /// distribution inside them.
    pub project_name: Option<String>,
    /// Where the packaging lives relative to the archive's top-level
    /// directory, or `None` when it is at the top. Monorepo snapshots put it
    /// in a subdirectory and the build has to change into it.
    pub project_dir: Option<String>,
    /// Sibling projects when the archive holds more than one, as
    /// `(directory, declared name)`.
    pub subprojects: Vec<(String, String)>,
    /// The archive that answered.
    pub archive_url: String,
}

/// Download a release archive and work out how it is built and what it calls
/// itself.
///
/// One extra download buys both, and both have to be settled before the
/// checkout is created — long before the spec is generated.
pub fn probe(repo_url: &str, version: &str, want: &str) -> Result<RepoProbe, String> {
    let candidates = archive_urls(repo_url, version);
    let mut last_err = None;

    for url in &candidates {
        let Ok(resp) = ureq::get(url).call() else {
            continue;
        };
        let mut buf = Vec::new();
        use std::io::Read;
        if resp
            .into_body()
            .into_reader()
            .read_to_end(&mut buf)
            .is_err()
            || buf.is_empty()
        {
            continue;
        }

        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join("archive.tar.gz");
        std::fs::write(&path, &buf).map_err(|e| e.to_string())?;
        let info = match ap_ecosystem::sdist::inspect_preferring(&path, Some(want)) {
            Ok(i) => i,
            Err(e) => {
                last_err = Some(format!("{url}: {e}"));
                continue;
            }
        };

        // Python packaging wins when both are present: a maturin project has
        // a Cargo.toml too, and it is a Python package that compiles Rust.
        let ecosystem = if info.python_packaging {
            Ecosystem::Python
        } else if info.cargo_manifest {
            Ecosystem::Rust
        } else if info.cmake_manifest || info.autotools_manifest {
            Ecosystem::PlainC
        } else {
            return Err(format!(
                "{url} contains no pyproject.toml, setup.py, Cargo.toml, \
                 CMakeLists.txt or configure.ac, so auto-package cannot tell \
                 how to build it"
            ));
        };

        return Ok(RepoProbe {
            ecosystem,
            project_name: info.project_name,
            project_dir: info.project_dir,
            subprojects: info
                .subprojects
                .iter()
                .map(|p| {
                    let name = p.name.clone().unwrap_or_else(|| p.dir.clone());
                    (p.dir.clone(), name)
                })
                .collect(),
            archive_url: url.clone(),
        });
    }

    // Provide helpful error with available tags
    let error_msg = match last_err {
        Some(e) => format!("could not read a release archive for {version}: {e}"),
        None => {
            let mut msg = format!(
                "no release archive for version {version} at {repo_url}. Tried:\n{}",
                candidates
                    .iter()
                    .map(|u| format!("  {u}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );

            // Add available tags to help user find the right version
            if let Ok(tags) = get_repo_tags(repo_url)
                && !tags.is_empty()
            {
                msg.push_str("\n\nAvailable tags (showing first 20):");
                for tag in tags.iter().take(20) {
                    let version = version_from_tag(tag, want);
                    msg.push_str(&format!("\n  {} -> version {}", tag, version));
                }
                msg.push_str("\n\nTry one of these versions with: --version <version>");
            }

            msg
        }
    };

    Err(error_msg)
}

/// The host, project path and repository name in a forge URL.
///
/// Delegates to [`ap_ecosystem::forge::parse`], which handles every forge
/// spelling the pipeline sees — HTTPS, `git@host:owner/repo.git` SSH,
/// `ssh://` URLs, nested GitLab groups (`group/subgroup/project`), and
/// forge sub-paths like `/tree/main` or `/-/blob/...`.
///
/// Returns `(host, project_path, repo_name)`:
/// - `host` — e.g. `github.com`, `gitlab.gnome.org`
/// - `project_path` — the full path below the host (`owner/repo` for
///   GitHub, `group/subgroup/project` for a nested GitLab group)
/// - `repo_name` — the trailing segment of the project path
fn owner_repo(repo_url: &str) -> Result<(String, String, String), String> {
    let forge = ap_ecosystem::forge::parse(repo_url)
        .ok_or_else(|| format!("{repo_url} is not a repository URL"))?;
    Ok((forge.host, forge.project_path, forge.repo_name))
}

/// Hand a GitHub API URL to the `gh` CLI if it's installed, otherwise
/// return `None` so the caller falls back to a raw HTTP call.
///
/// `gh api <path>` is strictly better than hand-rolling the HTTP call:
///
/// * It reads the token from `~/.config/gh/hosts.yml` (`gh auth login`),
///   so the user doesn't have to also keep `GITHUB_TOKEN` in their shell
///   env. The Oct 7 drogon-build report hit this: the user had `gh`
///   logged in but hadn't exported `GITHUB_TOKEN`, and the previous raw-
///   ureq-with-bearer-header path couldn't pick that up.
/// * It honors `gh`'s own 60-sec per-request-category rate-limit backoff
///   and retries the standard GitHub error-shape responses, so a
///   transient secondary rate limit no longer surfaces as a hard failure.
/// * It handles GitHub Enterprise automatically: `gh auth login`
///   remembers the host, and `gh api` dispatches accordingly without us
///   caring whether the URL is `api.github.com` or
///   `github.corp.example.com/api/v3`.
///
/// Non-GitHub URLs (gitlab, gitea, forgejo) stay on the raw HTTP path —
/// `gh` doesn't speak them.
fn try_gh_api(url: &str) -> Option<Result<String, String>> {
    // Only usable for GitHub API URLs. Keep the match deliberately strict
    // so an unrelated `api.github.com`-mentioning path (e.g. a
    // documentation link in a diag message that happens to come through
    // this function) doesn't get mis-routed.
    let path = url
        .strip_prefix("https://api.github.com/")
        .or_else(|| url.strip_prefix("http://api.github.com/"))?;

    // Resolve `gh` on PATH exactly once per process. Caching the "not
    // installed" result avoids spawning a doomed child subprocess on
    // every API call for the rest of the run.
    use std::sync::OnceLock;
    static GH_PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    let gh = GH_PATH
        .get_or_init(|| {
            // Trivial PATH scan — avoiding a `which` crate dep for one
            // callsite. `is_file()` is enough: if the entry isn't
            // executable, the subsequent `Command::new(gh).output()` will
            // just fail and we fall back to raw ureq like any other
            // `gh`-not-usable case.
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|p| p.join("gh"))
                    .find(|p| p.is_file())
            })
        })
        .as_ref()?;

    let out = std::process::Command::new(gh)
        .arg("api")
        // Pass the path verbatim including any query string — gh accepts
        // `repos/owner/repo/tags?per_page=1` directly.
        .arg(path)
        .output()
        .ok()?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Some(Err(format!(
            "gh api {path}: {}{}",
            if stderr.is_empty() {
                format!("exit {}", out.status.code().unwrap_or(-1))
            } else {
                stderr.trim().to_string()
            },
            // The two most common `gh api` failure shapes carry their own
            // fix-it hint — add one if it isn't already in stderr.
            if stderr.contains("401") || stderr.contains("authentication") {
                " (run `gh auth login` to refresh the token)"
            } else if stderr.contains("rate limit") || stderr.contains("secondary rate limit") {
                " (gh already retries on rate limits; this one persisted — \
                 wait, or re-run with --version to skip the API call)"
            } else {
                ""
            }
        )));
    }

    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    Some(Ok(body))
}

/// Fall-back auth for raw HTTP: a bearer token for GitHub, a PRIVATE-
/// TOKEN header for GitLab. Only used when `gh`/`glab` aren't on PATH or
/// the URL isn't one those CLIs can talk to.
fn forge_auth_header(url: &str) -> Option<(&'static str, String)> {
    if url.contains("api.github.com") || url.starts_with("https://api.github.com") {
        for var in &["GITHUB_TOKEN", "GH_TOKEN"] {
            if let Ok(tok) = std::env::var(var)
                && !tok.is_empty()
            {
                return Some(("Authorization", format!("Bearer {tok}")));
            }
        }
    } else if url.contains("gitlab") {
        for var in &["GITLAB_TOKEN", "CI_JOB_TOKEN"] {
            if let Ok(tok) = std::env::var(var)
                && !tok.is_empty()
            {
                return Some(("PRIVATE-TOKEN", tok));
            }
        }
    }
    None
}

/// Translate a bare `url: <ureq Error>` string into a message that names
/// the specific failure modes a forge API call usually hits.
///
/// Only reached on the raw-HTTP fall-back path — `gh` is strongly
/// preferred for GitHub and has its own friendlier errors (see
/// [`try_gh_api`]).
fn describe_forge_http_error(url: &str, raw: &str) -> String {
    let authed = forge_auth_header(url).is_some();
    if raw.contains("status: 403") || raw.contains("status: 429") {
        if authed {
            format!(
                "{url}: {raw} (your GITHUB_TOKEN/GITLAB_TOKEN is being \
                 rate-limited — wait or rotate to a token with a higher \
                 quota)"
            )
        } else if url.contains("api.github.com") {
            format!(
                "{url}: {raw} (GitHub unauthenticated REST is 60 req/hour \
                 per IP; install the `gh` CLI and run `gh auth login` so \
                 this process can use its auth automatically, or set \
                 GITHUB_TOKEN/GH_TOKEN, or pass --version to skip the \
                 API call)"
            )
        } else {
            format!(
                "{url}: {raw} (rate-limited; set GITLAB_TOKEN for \
                 authenticated access, or pass --version to skip the API \
                 call)"
            )
        }
    } else if raw.contains("status: 401") {
        format!(
            "{url}: {raw} (the authentication token this process has for \
             the forge was rejected — rotate it, or unset it to retry \
             unauthenticated)"
        )
    } else {
        format!("{url}: {raw}")
    }
}

/// `get_json`, exposed under a distinct name so cross-crate call sites
/// (currently `pipeline::search_github_repos`) can share the same
/// gh-preferring path without exposing the internal `get_json` name.
pub fn get_json_for_search(url: &str) -> Result<serde_json::Value, String> {
    get_json(url)
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    // Prefer `gh api` for GitHub URLs when it's installed — see
    // `try_gh_api`'s doc comment for the three reasons.
    if let Some(result) = try_gh_api(url) {
        let body = result?;
        return serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"));
    }

    let mut req = ureq::get(url)
        .header("User-Agent", "auto-package")
        .header("Accept", "application/json");
    if let Some((name, value)) = forge_auth_header(url) {
        req = req.header(name, value);
    }
    let body = req
        .call()
        .map_err(|e| describe_forge_http_error(url, &e.to_string()))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// The first `tag_name` (or `name`, for a bare tag list) in a forge response,
/// whether it answered with one object or an array of them.
fn first_tag(v: &serde_json::Value) -> Option<String> {
    let obj = match v {
        serde_json::Value::Array(items) => items.first()?,
        other => other,
    };
    obj.get("tag_name")
        .or_else(|| obj.get("name"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Ask the forge for the newest release tag of `repo_url`, and reduce it to a
/// version.
///
/// A repository has no package index behind it, so "latest" has to come from
/// the forge's own API. Releases are asked for first and plain tags second: a
/// project that tags but never cuts releases (common outside GitHub) still has
/// a newest version, and a project that does both means the release.
pub fn latest_release(repo_url: &str) -> Result<String, String> {
    let (host, project_path, repo_name) = owner_repo(repo_url)?;

    // Each forge family gets its release endpoint and its tag-list fallback.
    // `project_path` carries the full path below the host, so a nested GitLab
    // group (`group/subgroup/project`) is formatted correctly for the API.
    let endpoints: [String; 2] = if host.contains("github.com") {
        // GitHub has no subgroups, so project_path is always `owner/repo`.
        [
            format!("https://api.github.com/repos/{project_path}/releases/latest"),
            format!("https://api.github.com/repos/{project_path}/tags?per_page=1"),
        ]
    } else if host.contains("gitlab") {
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
    };

    let mut last_err = None;
    for url in &endpoints {
        match get_json(url) {
            Ok(v) => match first_tag(&v) {
                Some(tag) => return Ok(version_from_tag(&tag, &repo_name)),
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
        if v.len() > prefix.len() && v[..prefix.len()].eq_ignore_ascii_case(&prefix) {
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

/// Get all tags from a repository via the forge API.
fn get_repo_tags(repo_url: &str) -> Result<Vec<String>, String> {
    let (host, project_path, _repo_name) = owner_repo(repo_url)?;

    let api_url = if host.contains("github.com") {
        format!("https://api.github.com/repos/{project_path}/tags?per_page=100")
    } else if host.contains("gitlab") {
        let encoded = project_path.replace('/', "%2F");
        format!("https://{host}/api/v4/projects/{encoded}/repository/tags?per_page=100")
    } else {
        // Gitea/Forgejo
        format!("https://{host}/api/v1/repos/{project_path}/tags?limit=100")
    };

    let json = get_json(&api_url)?;

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
/// First tries exact match, then tries common patterns, then returns None.
fn find_matching_tag(tags: &[String], version: &str, repo: &str) -> Option<String> {
    // Try exact matches first
    for tag in tags {
        let normalized = version_from_tag(tag, repo);
        if normalized == version {
            return Some(tag.clone());
        }
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
    let base = repo_url.trim_end_matches('/').trim_end_matches(".git");
    let repo = base.rsplit('/').next().unwrap_or("source");
    let is_gitlab = base.contains("gitlab");

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
        .map(|tag| {
            if is_gitlab {
                format!("{base}/-/archive/{tag}/{repo}-{tag}.tar.gz")
            } else {
                format!("{base}/archive/refs/tags/{tag}.tar.gz")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// User-reported Oct 7 regression: `could not work out the latest
    /// version of https://github.com/drogonframework/drogon; ...
    /// api.github.com/.../tags?per_page=1: http status: 403`.
    ///
    /// 403 on the anon GitHub API means the IP has blown through the
    /// 60-req/hour cap. Error must mention the GITHUB_TOKEN remedy so
    /// the user doesn't have to go read the GitHub docs.
    #[test]
    fn describe_forge_error_names_the_github_token_remedy_on_403() {
        // Make sure neither env var is set so the "no auth, show the fix"
        // branch runs — tests run in parallel, so take a stable snapshot
        // of the env and restore it at the end (never panic across the
        // unsafe restore).
        let saved_gh = std::env::var("GH_TOKEN").ok();
        let saved_gh_token = std::env::var("GITHUB_TOKEN").ok();
        // SAFETY: single-threaded within this test; restored below.
        unsafe {
            std::env::remove_var("GH_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
        }

        let msg = describe_forge_http_error(
            "https://api.github.com/repos/drogonframework/drogon/tags?per_page=1",
            "http status: 403",
        );

        // Restore before any assertion that might panic.
        unsafe {
            if let Some(v) = saved_gh {
                std::env::set_var("GH_TOKEN", v);
            }
            if let Some(v) = saved_gh_token {
                std::env::set_var("GITHUB_TOKEN", v);
            }
        }

        assert!(
            msg.contains("GITHUB_TOKEN") && msg.contains("60 req/hour"),
            "403 must point the user at GITHUB_TOKEN and name the rate limit; got {msg:?}"
        );
    }

    /// With a token present, 403 means the token itself is being
    /// rate-limited (abuse detection, hitting the 5000/hour ceiling, or
    /// a secondary rate limit) — the fix is different, so the message
    /// must be different.
    #[test]
    fn describe_forge_error_hints_rotate_token_when_authed() {
        let saved = std::env::var("GITHUB_TOKEN").ok();
        unsafe {
            std::env::set_var("GITHUB_TOKEN", "ghp_fake");
        }

        let msg = describe_forge_http_error(
            "https://api.github.com/repos/x/y/releases/latest",
            "http status: 403",
        );

        unsafe {
            match saved {
                Some(v) => std::env::set_var("GITHUB_TOKEN", v),
                None => std::env::remove_var("GITHUB_TOKEN"),
            }
        }

        assert!(
            msg.contains("rate-limited") && msg.contains("rotate"),
            "authed 403 must suggest wait/rotate; got {msg:?}"
        );
    }

    #[test]
    fn describe_forge_error_leaves_non_auth_errors_alone() {
        let msg = describe_forge_http_error(
            "https://api.github.com/repos/x/y/tags?per_page=1",
            "http status: 500",
        );
        assert!(
            msg.contains("500") && !msg.contains("GITHUB_TOKEN"),
            "non-auth errors stay a raw passthrough; got {msg:?}"
        );
    }

    #[test]
    fn lookup_ignores_name_spelling() {
        let mut m = SourceMap::default();
        m.insert("nvidia-ml-py", "https://github.com/nvidia/nvidia-ml-py");
        assert!(m.get("nvidia_ml_py").is_some());
        assert!(m.get("NVIDIA-ML-PY").is_some());
        assert!(m.get("nvidia-ml").is_none());
    }

    #[test]
    fn a_bare_url_applies_to_the_package_being_built() {
        let (pkg, url) =
            SourceMap::parse_arg("https://github.com/x/y", "nvidia-cuda-runtime").unwrap();
        assert_eq!(pkg, "nvidia-cuda-runtime");
        assert_eq!(url, "https://github.com/x/y");
    }

    #[test]
    fn an_explicit_package_wins_over_the_default() {
        let (pkg, url) =
            SourceMap::parse_arg("cuda-python=https://github.com/x/y", "root").unwrap();
        assert_eq!(pkg, "cuda-python");
        assert_eq!(url, "https://github.com/x/y");
    }

    #[test]
    fn a_value_that_is_neither_is_rejected_rather_than_guessed() {
        assert!(SourceMap::parse_arg("just-a-name", "root").is_err());
        assert!(SourceMap::parse_arg("pkg=", "root").is_err());
    }

    #[test]
    fn gitlab_uses_its_own_archive_layout() {
        let urls = archive_urls("https://gitlab.com/owner/thing", "1.2.3");
        assert!(
            urls[0].contains("/-/archive/v1.2.3/thing-v1.2.3.tar.gz"),
            "{urls:?}"
        );

        let urls = archive_urls("https://github.com/owner/thing.git", "1.2.3");
        assert_eq!(
            urls[0],
            "https://github.com/owner/thing/archive/refs/tags/v1.2.3.tar.gz"
        );
    }

    #[test]
    fn both_url_spellings_split_the_same_way() {
        for url in [
            "https://github.com/NVIDIA/cuda-python",
            "https://github.com/NVIDIA/cuda-python.git",
            "https://github.com/NVIDIA/cuda-python/",
            "git@github.com:NVIDIA/cuda-python.git",
        ] {
            let (host, project_path, repo) = owner_repo(url).unwrap();
            assert_eq!(
                (host.as_str(), project_path.as_str(), repo.as_str()),
                ("github.com", "NVIDIA/cuda-python", "cuda-python"),
                "{url}",
            );
        }
        assert!(owner_repo("https://github.com/NVIDIA").is_err());
    }

    #[test]
    fn gitlab_nested_group_is_preserved_through_owner_repo() {
        // The whole project path survives — `group/subgroup/project`, not
        // just the first two segments. latest_release's GitLab branch then
        // URL-encodes it correctly for the API.
        let (host, project_path, repo) =
            owner_repo("https://gitlab.com/group/subgroup/project").unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(project_path, "group/subgroup/project");
        assert_eq!(repo, "project");
    }

    #[test]
    fn ssh_scp_style_normalises_before_api_calls() {
        // Even the SSH spelling resolves to the same (host, project_path,
        // repo) — downstream HTTPS API calls work transparently.
        let (host, project_path, _) =
            owner_repo("git@gitlab.com:group/subgroup/project.git").unwrap();
        assert_eq!(host, "gitlab.com");
        assert_eq!(project_path, "group/subgroup/project");
    }

    #[test]
    fn a_tag_is_reduced_to_the_version_it_carries() {
        assert_eq!(version_from_tag("v12.9.0", "cuda-python"), "12.9.0");
        assert_eq!(version_from_tag("12.9.0", "cuda-python"), "12.9.0");
        assert_eq!(
            version_from_tag("cuda-python-v12.9.0", "cuda-python"),
            "12.9.0"
        );
        assert_eq!(version_from_tag("release-1.2", "thing"), "1.2");
        // Not a version prefix: a tag that merely starts with a letter v.
        assert_eq!(version_from_tag("voyager", "thing"), "voyager");
    }

    #[test]
    fn a_resolved_version_round_trips_back_to_its_tag() {
        let tag = "v12.9.0";
        let version = version_from_tag(tag, "cuda-python");
        let urls = archive_urls("https://github.com/NVIDIA/cuda-python", &version);
        assert!(urls[0].ends_with(&format!("/{tag}.tar.gz")), "{urls:?}");
    }

    #[test]
    fn a_forge_answer_yields_its_tag_whether_wrapped_in_an_array_or_not() {
        let one: serde_json::Value = serde_json::json!({"tag_name": "v1.0.0"});
        assert_eq!(first_tag(&one).as_deref(), Some("v1.0.0"));
        let many: serde_json::Value = serde_json::json!([{"name": "v2.0.0"}, {"name": "v1.0.0"}]);
        assert_eq!(first_tag(&many).as_deref(), Some("v2.0.0"));
        assert_eq!(first_tag(&serde_json::json!([])), None);
    }

    #[test]
    fn rust_crates_are_not_given_the_python_prefix() {
        assert_eq!(Ecosystem::Rust.rpm_name("ripgrep"), "ripgrep");
        assert_eq!(
            Ecosystem::Python.rpm_name("MarkupSafe"),
            "python-markupsafe"
        );
        assert_eq!(Ecosystem::PlainC.rpm_name("nlohmann-json"), "nlohmann-json");
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SourceMap::default();
        m.insert("cuda-python", "https://github.com/NVIDIA/cuda-python");
        m.save(dir.path()).unwrap();
        let back = SourceMap::load(dir.path()).unwrap();
        assert_eq!(
            back.get("cuda_python"),
            Some("https://github.com/NVIDIA/cuda-python")
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_map_but_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SourceMap::load(dir.path()).unwrap().is_empty());
        std::fs::write(SourceMap::path(dir.path()), "this is not toml {{{").unwrap();
        assert!(SourceMap::load(dir.path()).is_err());
    }
}
