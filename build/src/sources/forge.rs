use super::*;

/// Download a release archive and work out how it is built and what it calls
/// itself.
///
/// One extra download buys both, and both have to be settled before the
/// checkout is created — long before the spec is generated.
pub fn probe(repo_url: &str, version: &str, want: &str) -> Result<RepoProbe, String> {
    let candidates = archive_urls(repo_url, version);
    // Candidates that exist but could not be used, in order. A 404 is not one
    // of them: that URL is simply not this release's archive.
    let mut failures: Vec<String> = Vec::new();

    for url in &candidates {
        let path = match fetch_archive(url) {
            Ok(path) => path,
            Err(FetchFailure::NotFound) => continue,
            Err(e) => {
                failures.push(format!("{url}: {e}"));
                continue;
            }
        };
        let info = match ap_ecosystem::sdist::inspect_preferring(&path, Some(want)) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("{url}: {e}"));
                evict_archive(url);
                continue;
            }
        };

        // Python packaging wins when both are present: a maturin project has
        // a Cargo.toml too, and it is a Python package that compiles Rust.
        //
        // EXCEPTION — see `is_python_hijack`.
        let native_root = info.cmake_at_root || info.autotools_at_root;
        let python_hijack = is_python_hijack(&info, want);
        let ecosystem = if info.python_packaging && !python_hijack {
            Ecosystem::Python
        } else if info.cargo_manifest {
            Ecosystem::Rust
        } else if native_root {
            Ecosystem::PlainC
        } else {
            return Err(format!(
                "{url} contains no pyproject.toml, setup.py, Cargo.toml, \
                 CMakeLists.txt or configure.ac, so auto-package cannot tell \
                 how to build it"
            ));
        };

        // Under the Python-hijack exception, `info.project_name` still
        // holds the pyproject.toml's declared name (`llama-cpp-scripts`
        // etc.) — surfacing it would make cli/src/main.rs's auto-rename
        // flip the user's request to that name anyway, defeating the
        // whole point of picking PlainC here. Suppress it so the caller
        // keeps the user-supplied name.
        let project_name = if python_hijack {
            None
        } else {
            info.project_name
        };
        return Ok(RepoProbe {
            ecosystem,
            project_name,
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
    let error_msg = if !failures.is_empty() {
        format!(
            "could not read a release archive for {version}:\n  {}",
            failures.join("\n  ")
        )
    } else {
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
pub(super) fn owner_repo(repo_url: &str) -> Result<(String, String, String), String> {
    let forge = ap_ecosystem::forge::parse(repo_url)
        .ok_or_else(|| format!("{repo_url} is not a repository URL"))?;
    // These go into API paths and archive URLs. Anything beyond a plain name
    // (a `?`, a `%` escape, or `.`/`..`, which a URL resolves) could point the
    // request at another endpoint. A nested GitLab path is checked segment by
    // segment.
    let name_ok = |s: &str| {
        s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let host_ok = forge
        .host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    if !host_ok || !forge.project_path.split('/').all(name_ok) {
        return Err(format!(
            "{repo_url} does not name a forge owner and repository"
        ));
    }
    Ok((forge.host, forge.project_path, forge.repo_name))
}

/// Whether `path` (after `https://api.github.com/`) is one of the requests this
/// crate makes: a repository's latest release or tags, or a repository search.
pub(super) fn gh_api_path_ok(path: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"^(?:repos/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/(?:releases/latest|tags)|search/repositories)(?:\?[A-Za-z0-9%_=&.+~-]*)?$",
        )
        .unwrap()
    })
    .is_match(path)
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
pub(super) fn try_gh_api(url: &str) -> Option<Result<String, String>> {
    // Only usable for GitHub API URLs. Keep the match deliberately strict
    // so an unrelated `api.github.com`-mentioning path (e.g. a
    // documentation link in a diag message that happens to come through
    // this function) doesn't get mis-routed.
    let path = url.strip_prefix("https://api.github.com/")?;
    // The path becomes an argument to `gh api`, which also takes `-X`,
    // `--hostname` and the like, and may name any endpoint the token reaches.
    // Only the request shapes this crate makes are passed on.
    if !gh_api_path_ok(path) {
        return None;
    }

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

    // Bounded like the raw HTTP path: a hung `gh` must not stall the run. A
    // timeout returns None, and the caller falls back to raw HTTP.
    let out = crate::proc::capture_with_timeout_env(
        gh.to_str()?,
        // Pass the path verbatim including any query string — gh accepts
        // `repos/owner/repo/tags?per_page=1` directly.
        &["api", path],
        // `gh` reads `.git` from where it starts; not the shared temp dir.
        crate::proc::private_dir().ok()?,
        crate::proc::Emit::quiet(),
        std::time::Duration::from_secs(60),
        // `gh` finds its token in its own config, `GH_*` or `GITHUB_*`.
        Some(&crate::proc::minimal_env(&["GH_", "GITHUB_"])),
    )
    .ok()?;

    if out.code != 0 {
        let stderr = out.stderr.trim().to_string();
        return Some(Err(format!(
            "gh api {path}: {}{}",
            if stderr.is_empty() {
                format!("exit {}", out.code)
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

    Some(Ok(out.stdout))
}

/// Fall-back auth for raw HTTP: a bearer token for GitHub, a PRIVATE-TOKEN
/// (or JOB-TOKEN) header for GitLab. Only used when `gh`/`glab` aren't on PATH or
/// the URL isn't one those CLIs can talk to.
pub(super) fn forge_auth_header(url: &str) -> Option<(&'static str, String)> {
    // Decide on the parsed HOST, never a substring: `https://evil.com/api.github.com/x`
    // or `https://gitlab-mirror.evil.com` must not receive a token. https only.
    if !url.starts_with("https://") {
        return None;
    }
    let host = crate::http::host_of(url)?;
    let first_set = |vars: &[&str]| {
        vars.iter()
            .find_map(|v| std::env::var(v).ok().filter(|t| !t.is_empty()))
    };
    if host == "api.github.com" {
        return first_set(&["GITHUB_TOKEN", "GH_TOKEN"])
            .map(|tok| ("Authorization", format!("Bearer {tok}")));
    }
    let self_hosted = std::env::var("GITLAB_HOST").ok().map(|h| {
        h.trim_start_matches("https://")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    });
    if host == "gitlab.com" || self_hosted.as_deref() == Some(host.as_str()) {
        return gitlab_auth(|v| first_set(&[v]));
    }
    None
}

/// `GITLAB_TOKEN` is a personal/project token (`PRIVATE-TOKEN`); `CI_JOB_TOKEN`
/// is a job token and GitLab only accepts it in `JOB-TOKEN`.
pub(super) fn gitlab_auth(var: impl Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    var("GITLAB_TOKEN")
        .map(|tok| ("PRIVATE-TOKEN", tok))
        .or_else(|| var("CI_JOB_TOKEN").map(|tok| ("JOB-TOKEN", tok)))
}

/// Why a request to a forge API failed, from the status and headers of the
/// answer rather than from the text of an error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// 404.
    NotFound,
    /// 401: the credential sent was rejected.
    Unauthorized,
    /// 429, or 403 with the quota used up (`x-ratelimit-remaining: 0`).
    RateLimited {
        status: u16,
        /// The wait the server asked for, in seconds, when it said.
        retry_after: Option<u64>,
    },
    /// Any other error status.
    Status(u16),
    /// No usable answer: connection, timeout, body, redirect or local failure.
    Transport(String),
}

impl ForgeError {
    /// From the final error answer of a retried call.
    pub(super) fn from_status_failure(f: &ap_ecosystem::net::StatusFailure) -> Self {
        if f.is_rate_limit() {
            return ForgeError::RateLimited {
                status: f.status,
                retry_after: f.headers.retry_after,
            };
        }
        Self::from_status(f.status)
    }

    pub(super) fn from_status(status: u16) -> Self {
        match status {
            404 => ForgeError::NotFound,
            401 => ForgeError::Unauthorized,
            429 => ForgeError::RateLimited {
                status,
                retry_after: None,
            },
            s => ForgeError::Status(s),
        }
    }

    fn from_call_failure(f: &ap_ecosystem::net::CallFailure) -> Self {
        match f {
            ap_ecosystem::net::CallFailure::Status(s) => Self::from_status_failure(s),
            ap_ecosystem::net::CallFailure::Transport(e) => Self::from_ureq(e),
        }
    }

    pub(super) fn from_ureq(e: &ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(s) => Self::from_status(*s),
            other => ForgeError::Transport(other.to_string()),
        }
    }

    /// 403 and 429 both mean "refused for now" to the user, whether or not the
    /// answer said the quota was spent.
    fn is_refusal(&self) -> bool {
        matches!(
            self,
            ForgeError::RateLimited { .. } | ForgeError::Status(403)
        )
    }
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::NotFound => write!(f, "http status: 404"),
            ForgeError::Unauthorized => write!(f, "http status: 401"),
            ForgeError::RateLimited {
                status,
                retry_after: Some(s),
            } => write!(f, "http status: {status} (retry after {s}s)"),
            ForgeError::RateLimited { status, .. } => write!(f, "http status: {status}"),
            ForgeError::Status(s) => write!(f, "http status: {s}"),
            ForgeError::Transport(e) => write!(f, "{e}"),
        }
    }
}

/// Translate a forge API failure into a message that names the specific
/// failure modes a forge API call usually hits.
///
/// Only reached on the raw-HTTP fall-back path — `gh` is strongly
/// preferred for GitHub and has its own friendlier errors (see
/// [`try_gh_api`]).
pub(super) fn describe_forge_http_error(url: &str, err: &ForgeError) -> String {
    let authed = forge_auth_header(url).is_some();
    let github = crate::http::host_of(url).as_deref() == Some("api.github.com");
    if err.is_refusal() {
        if authed {
            format!(
                "{url}: {err} (your GITHUB_TOKEN/GITLAB_TOKEN is being \
                 rate-limited — wait or rotate to a token with a higher \
                 quota)"
            )
        } else if github {
            format!(
                "{url}: {err} (GitHub unauthenticated REST is 60 req/hour \
                 per IP; install the `gh` CLI and run `gh auth login` so \
                 this process can use its auth automatically, or set \
                 GITHUB_TOKEN/GH_TOKEN, or pass --version to skip the \
                 API call)"
            )
        } else {
            format!(
                "{url}: {err} (rate-limited; set GITLAB_TOKEN for \
                 authenticated access, or pass --version to skip the API \
                 call)"
            )
        }
    } else if *err == ForgeError::Unauthorized {
        format!(
            "{url}: {err} (the authentication token this process has for \
             the forge was rejected — rotate it, or unset it to retry \
             unauthenticated)"
        )
    } else {
        format!("{url}: {err}")
    }
}

/// `get_json`, exposed under a distinct name so cross-crate call sites
/// (currently `pipeline::search_github_repos`) can share the same
/// gh-preferring path without exposing the internal `get_json` name.
pub fn get_json_for_search(url: &str) -> Result<serde_json::Value, String> {
    get_json(url)
}

pub(super) fn get_json(url: &str) -> Result<serde_json::Value, String> {
    // Prefer `gh api` for GitHub URLs when it's installed — see
    // `try_gh_api`'s doc comment for the three reasons.
    if let Some(result) = try_gh_api(url) {
        let body = result?;
        return serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"));
    }

    let body = http_get_text(url)?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// GET `url` as JSON text with timeout and size cap. A token is attached only
/// for the hosts [`forge_auth_header`] allows, and redirects are followed by
/// hand so it is never carried to a different host (ureq would forward a
/// custom header like `PRIVATE-TOKEN` across any redirect).
pub(super) fn http_get_text(url: &str) -> Result<String, String> {
    let agent = crate::http::agent(crate::http::API_TIMEOUT, 0);
    let mut current = url.to_string();
    for _ in 0..5 {
        let origin_host = crate::http::host_of(url);
        let auth = if crate::http::host_of(&current) == origin_host {
            forge_auth_header(&current)
        } else {
            None
        };
        let resp = ap_ecosystem::net::call_with_retries_detailed(|| {
            let mut req = agent
                .get(&current)
                .header("User-Agent", "auto-package")
                .header("Accept", "application/json");
            if let Some((name, value)) = &auth {
                req = req.header(*name, value.as_str());
            }
            req
        })
        .map_err(|e| describe_forge_http_error(url, &ForgeError::from_call_failure(&e)))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status)
            && let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok())
        {
            current = redirect_target(&current, loc).map_err(|e| format!("{url}: {e}"))?;
            continue;
        }
        if status >= 400 {
            return Err(describe_forge_http_error(
                url,
                &ForgeError::from_status(status),
            ));
        }
        return crate::http::read_text(resp).map_err(|e| format!("{url}: {e}"));
    }
    Err(format!("{url}: too many redirects"))
}

/// Where a redirect from `current` to `loc` leads. An https URL may not be
/// redirected to plain http: that would send the request (and any token) in
/// the clear, and a network attacker could forge the answer.
pub(super) fn redirect_target(current: &str, loc: &str) -> Result<String, String> {
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(loc.to_string());
    }
    if lower.starts_with("http://") {
        if current.to_ascii_lowercase().starts_with("https://") {
            return Err(format!("refusing redirect from https to insecure {loc}"));
        }
        return Ok(loc.to_string());
    }
    let host = crate::http::host_of(current).unwrap_or_default();
    Ok(format!("https://{host}{loc}"))
}

/// The first `tag_name` (or `name`, for a bare tag list) in a forge response,
/// whether it answered with one object or an array of them.
pub(super) fn first_tag(v: &serde_json::Value) -> Option<String> {
    let obj = match v {
        serde_json::Value::Array(items) => items.first()?,
        other => other,
    };
    obj.get("tag_name")
        .or_else(|| obj.get("name"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// The repository name a forge answer (a release, or a tag list) says it comes
/// from, read from its `tarball_url` (`.../repos/<owner>/<name>/tarball/<tag>`).
/// A repository that was renamed or merged into another answers under the new
/// name, so a mismatch with the name asked for means the releases are another
/// project's.
pub(super) fn answered_repo_name(v: &serde_json::Value) -> Option<String> {
    let obj = match v {
        serde_json::Value::Array(items) => items.first()?,
        other => other,
    };
    let url = obj.get("tarball_url")?.as_str()?;
    let after = url.split("/repos/").nth(1)?;
    let mut parts = after.split('/');
    let _owner = parts.next()?;
    parts.next().map(str::to_string)
}
