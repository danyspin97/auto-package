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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::http::FetchFailure;

/// Package name (normalized) → source repository URL.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceMap {
    #[serde(default, rename = "source")]
    urls: BTreeMap<String, String>,
}

/// PEP 503 normalisation: lowercase, and every run of `-`, `_` and `.`
/// becomes a single `-`. Shared so every lookup treats `Foo_Bar`, `foo.bar`
/// and `foo--bar` alike.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !in_sep {
                out.push('-');
            }
            in_sep = true;
        } else {
            in_sep = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Map key for a package. Spelled with `_` rather than `-` so entries already
/// persisted in `sources.toml` keep matching.
fn key(name: &str) -> String {
    normalize_name(name).replace('-', "_")
}

/// The key earlier versions stored: lowercase, `-`/`.` → `_`, with runs of
/// separators left alone. Entries in an existing `sources.toml` for names such
/// as `foo--bar` live under `foo__bar`, and collapsing runs would orphan them.
fn legacy_key(name: &str) -> String {
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
        self.urls
            .get(&legacy_key(pkg))
            .or_else(|| self.urls.get(&key(pkg)))
            .map(String::as_str)
    }

    pub fn insert(&mut self, pkg: &str, url: &str) {
        // Overwrite an entry stored under the old spelling rather than
        // leaving a stale duplicate that `get` would still find first.
        let k = if self.urls.contains_key(&legacy_key(pkg)) {
            legacy_key(pkg)
        } else {
            key(pkg)
        };
        self.urls
            .insert(k, url.trim().trim_end_matches('/').to_string());
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

/// Should the probe treat this archive as a C/C++ project despite the
/// presence of a `pyproject.toml` at the same root?
///
/// Returns `true` when every one of these holds:
///   - the archive has native build-system files at the root
///     (`CMakeLists.txt`, `configure.ac`, …);
///   - the archive has a `pyproject.toml` (so the default rule would
///     normally pick Python);
///   - there is NO `Cargo.toml` at the root (a maturin project genuinely
///     IS a Python-primary build with a Rust extension, so we leave it
///     alone);
///   - the `pyproject.toml` declares a name that doesn't match (after
///     the usual normalization) the name the user asked for.
///
/// The llama.cpp motivator: `CMakeLists.txt` + `pyproject.toml` at the
/// root, where the pyproject declares `name = "llama-cpp-scripts"` for
/// shipping Python helper scripts. The user asked for `llama-cpp` (the
/// C++ library). Without this exception the auto-rename in
/// `cli/src/main.rs` quietly flips `llama-cpp` → `llama-cpp-scripts`,
/// the pipeline goes down the Python path, py2pack 404s on PyPI, and
/// the whole run dies with "project not on PyPI" and no mention of C++
/// at all — exactly the drogon-shape mis-trip the plain-c-cpp skill
/// was supposed to catch.
pub(crate) fn is_python_hijack(info: &ap_ecosystem::sdist::SdistInfo, want: &str) -> bool {
    let native_root = info.cmake_at_root || info.autotools_at_root;
    if !(info.python_packaging && native_root && !info.cargo_manifest) {
        return false;
    }
    match info.project_name.as_deref() {
        Some(declared) => {
            let normalize = |s: &str| s.to_lowercase().replace(['-', '.'], "_");
            normalize(declared) != normalize(want)
        }
        // No name declared anywhere, and the Python packaging sits in a
        // SUBDIRECTORY of a CMake/autotools root: that is a C/C++ library that
        // ships a Python binding under `python/` (google/re2, protobuf-style
        // trees), not a Python project with a native sidecar. Python
        // packaging at the root with no declared name stays Python.
        None => info.project_dir.is_some(),
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

/// Archives fetched this run, by URL. The probe downloads the archive it
/// inspects; the spec generator takes that copy rather than fetching it again.
#[derive(Default)]
struct ArchiveCache {
    files: std::collections::HashMap<String, PathBuf>,
    /// URLs that answered 404, so they are not asked for again this run.
    missing: std::collections::HashSet<String>,
}

fn archive_cache() -> &'static Mutex<ArchiveCache> {
    static CACHE: OnceLock<Mutex<ArchiveCache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Download `url` to `dest`, noting a 404 in the cache.
fn download_archive(url: &str, dest: &Path) -> Result<(), FetchFailure> {
    let result = crate::http::download_file(url, dest);
    if let Err(FetchFailure::NotFound) = result {
        archive_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .missing
            .insert(url.to_string());
    }
    let download = result?;
    ap_core::diag!(
        "  fetched {url} ({} bytes, sha256 {})",
        download.bytes,
        download.sha256
    );
    Ok(())
}

/// The archive at `url` as a file in this process's scratch directory, reused
/// when it was already fetched this run.
pub fn fetch_archive(url: &str) -> Result<PathBuf, FetchFailure> {
    {
        let cache = archive_cache().lock().unwrap_or_else(|e| e.into_inner());
        if cache.missing.contains(url) {
            return Err(FetchFailure::NotFound);
        }
        if let Some(path) = cache.files.get(url).filter(|p| p.is_file()) {
            return Ok(path.clone());
        }
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("ap-archives-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| FetchFailure::Transport(e.to_string()))?;
    let base = url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|b| !b.is_empty())
        .unwrap_or("archive");
    let path = dir.join(format!("{}-{base}", NEXT.fetch_add(1, Ordering::Relaxed)));
    download_archive(url, &path)?;
    archive_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .files
        .insert(url.to_string(), path.clone());
    Ok(path)
}

/// Put the archive at `url` at `dest`: the copy the probe fetched when there is
/// one (moved there, so it is not kept twice), otherwise a fresh download.
pub fn fetch_archive_to(url: &str, dest: &Path) -> Result<(), FetchFailure> {
    let cached = {
        let mut cache = archive_cache().lock().unwrap_or_else(|e| e.into_inner());
        if cache.missing.contains(url) {
            return Err(FetchFailure::NotFound);
        }
        cache.files.remove(url)
    };
    if let Some(path) = cached.filter(|p| p.is_file())
        && std::fs::copy(&path, dest).is_ok()
    {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    download_archive(url, dest)
}

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
                let _ = std::fs::remove_file(&path);
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

    // Bounded like the raw HTTP path: a hung `gh` must not stall the run. A
    // timeout returns None, and the caller falls back to raw HTTP.
    let out = crate::proc::capture_with_timeout_env(
        gh.to_str()?,
        // Pass the path verbatim including any query string — gh accepts
        // `repos/owner/repo/tags?per_page=1` directly.
        &["api", path],
        &std::env::temp_dir(),
        crate::proc::Emit::quiet(),
        std::time::Duration::from_secs(60),
        None,
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

/// Fall-back auth for raw HTTP: a bearer token for GitHub, a PRIVATE-
/// TOKEN header for GitLab. Only used when `gh`/`glab` aren't on PATH or
/// the URL isn't one those CLIs can talk to.
fn forge_auth_header(url: &str) -> Option<(&'static str, String)> {
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
        return first_set(&["GITLAB_TOKEN", "CI_JOB_TOKEN"]).map(|tok| ("PRIVATE-TOKEN", tok));
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

    let body = http_get_text(url)?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// GET `url` as JSON text with timeout and size cap. A token is attached only
/// for the hosts [`forge_auth_header`] allows, and redirects are followed by
/// hand so it is never carried to a different host (ureq would forward a
/// custom header like `PRIVATE-TOKEN` across any redirect).
fn http_get_text(url: &str) -> Result<String, String> {
    let agent = crate::http::agent(crate::http::API_TIMEOUT, 0);
    let mut current = url.to_string();
    for _ in 0..5 {
        let mut req = agent
            .get(&current)
            .header("User-Agent", "auto-package")
            .header("Accept", "application/json");
        let origin_host = crate::http::host_of(url);
        if crate::http::host_of(&current) == origin_host
            && let Some((name, value)) = forge_auth_header(&current)
        {
            req = req.header(name, value);
        }
        let resp = req
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|e| describe_forge_http_error(url, &e.to_string()))?;
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
                &format!("http status: {status}"),
            ));
        }
        return crate::http::read_text(resp).map_err(|e| format!("{url}: {e}"));
    }
    Err(format!("{url}: too many redirects"))
}

/// Where a redirect from `current` to `loc` leads. An https URL may not be
/// redirected to plain http: that would send the request (and any token) in
/// the clear, and a network attacker could forge the answer.
fn redirect_target(current: &str, loc: &str) -> Result<String, String> {
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

// ── non-forge sources: release directories and direct tarballs ─────────────────
//
// Plenty of upstreams (GNU Savannah, ftp.gnu.org, self-hosted project sites)
// have no forge API at all: just a directory of release tarballs. They are
// recognised by shape, not by a hard-coded host list:
//
// * a URL whose last segment is a tarball is a *direct archive*;
// * a URL on an unknown host with an index-like path segment
//   (`/releases/`, `/pub/`, `/gnu/`, ...) is a *release index* — an HTML
//   directory listing whose links are the tarballs.
//
// Only `.tar.gz`/`.tgz` are used: every later stage stores and extracts the
// download as gzip, and an `.xz`-only release is reported as unsupported
// rather than silently mis-extracted.

/// How a source URL is to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A git forge with a REST API (GitHub, GitLab, Gitea/Forgejo).
    Forge,
    /// A directory listing of release tarballs.
    ReleaseIndex,
    /// One specific release tarball.
    DirectArchive,
}

const GZIP_EXTS: &[&str] = &[".tar.gz", ".tgz"];
const OTHER_ARCHIVE_EXTS: &[&str] = &[
    ".tar.xz", ".txz", ".tar.lz", ".tar.bz2", ".tbz2", ".tar.zst", ".zip",
];
const INDEX_HINTS: &[&str] = &[
    "releases",
    "release",
    "pub",
    "gnu",
    "files",
    "download",
    "downloads",
    "dist",
    "distfiles",
    "sources",
    "src",
    "ftp",
    "mirror",
    "archive",
];

fn last_segment(url: &str) -> &str {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
}

fn is_gzip_archive(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    GZIP_EXTS.iter().any(|e| f.ends_with(e))
}

fn is_any_archive(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    GZIP_EXTS
        .iter()
        .chain(OTHER_ARCHIVE_EXTS)
        .any(|e| f.ends_with(e))
}

/// Classify a source URL. See the section comment above.
pub fn source_kind(url: &str) -> SourceKind {
    // A file under `/releases/download/` or `/archive/` is a fixed asset: the
    // forge-first rule below would turn it into a lookup of the latest tag and
    // package a different version than the URL names.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if (path.contains("/releases/download/") || path.contains("/archive/"))
        && is_any_archive(last_segment(url))
    {
        return SourceKind::DirectArchive;
    }
    // Other forge URLs (repositories, tags) before archive extensions, so a
    // tag URL is still resolved through the forge.
    if let Some(forge) = ap_ecosystem::forge::parse(url)
        && ap_ecosystem::forge::is_known_host(&forge.host)
    {
        return SourceKind::Forge;
    }
    if is_any_archive(last_segment(url)) {
        return SourceKind::DirectArchive;
    }
    let Some(forge) = ap_ecosystem::forge::parse(url) else {
        return SourceKind::Forge;
    };
    let indexish = forge
        .project_path
        .split('/')
        .any(|seg| INDEX_HINTS.contains(&seg.to_ascii_lowercase().as_str()));
    if indexish {
        SourceKind::ReleaseIndex
    } else {
        SourceKind::Forge
    }
}

/// `(name, version)` from a release tarball filename: `foo-1.2.3.tar.gz` →
/// `("foo", "1.2.3")`. The split is at the last `-` or `_` that is followed
/// by a digit, so `foo-bar-2.0.tar.gz` → `("foo-bar", "2.0")`.
pub fn archive_name_version(file: &str) -> Option<(String, String)> {
    let lower = file.to_ascii_lowercase();
    let ext = GZIP_EXTS
        .iter()
        .chain(OTHER_ARCHIVE_EXTS)
        .find(|e| lower.ends_with(**e))?;
    let stem = &file[..file.len() - ext.len()];
    let idx = stem
        .char_indices()
        .rev()
        .find(|&(i, c)| {
            (c == '-' || c == '_') && stem[i + 1..].starts_with(|d: char| d.is_ascii_digit())
        })?
        .0;
    let (name, version) = (&stem[..idx], &stem[idx + 1..]);
    (!name.is_empty()).then(|| (name.to_string(), version.to_string()))
}

/// Link targets of an HTML directory listing, reduced to their last path
/// segment. Queries, anchors, parent/sort links and absolute foreign links
/// are dropped; entity-escaped ampersands are not an issue because only the
/// file name survives.
pub fn parse_index_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Lowercased once: ASCII lowercasing keeps byte offsets, and doing it per
    // link made a large listing quadratic.
    let lower = html.to_ascii_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("href=") {
        let after = &html[pos + i + 5..];
        let (quote, body) = match after.chars().next() {
            Some(q @ ('"' | '\'')) => (q, &after[1..]),
            _ => {
                pos += i + 5;
                continue;
            }
        };
        let Some(end) = body.find(quote) else { break };
        let target = &body[..end];
        pos = html.len() - body.len() + end;
        let file = last_segment(target);
        if !file.is_empty()
            && !target.contains("..")
            && !target.starts_with('?')
            && !target.starts_with('#')
            && seen.insert(file.to_string())
        {
            out.push(file.to_string());
        }
    }
    out
}

/// Component-wise version comparison: numeric parts compare as numbers
/// (`1.10 > 1.9`), anything else lexically. A shorter version that is a
/// prefix of a longer one sorts first.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |v: &str| -> Vec<String> { v.split(['.', '-', '_']).map(str::to_string).collect() };
    let (pa, pb) = (split(a), split(b));
    for (x, y) in pa.iter().zip(&pb) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len())
}

fn is_prerelease(version: &str) -> bool {
    let v = version.to_ascii_lowercase();
    ["rc", "alpha", "beta", "pre", "dev", "snapshot", "git"]
        .iter()
        .any(|m| v.contains(m))
}

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

fn fetch_index(index_url: &str) -> Result<Vec<String>, String> {
    let body = crate::http::read_text(
        crate::http::get(index_url)
            .header("User-Agent", "auto-package")
            .call()
            .map_err(|e| format!("{index_url}: {e}"))?,
    )
    .map_err(|e| format!("{index_url}: {e}"))?;
    Ok(parse_index_links(&body))
}

fn index_dir(index_url: &str) -> String {
    format!(
        "{}/",
        index_url
            .split(['?', '#'])
            .next()
            .unwrap_or(index_url)
            .trim_end_matches('/')
    )
}

fn latest_release_non_forge(url: &str, kind: SourceKind) -> Result<String, String> {
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
fn same_version(a: &str, b: &str) -> bool {
    fn canon(v: &str) -> &str {
        let mut v = v.strip_prefix(['v', 'V']).unwrap_or(v);
        while let Some(rest) = v.strip_suffix(".0") {
            v = rest;
        }
        v
    }
    canon(a) == canon(b)
}

fn archive_urls_non_forge(url: &str, kind: SourceKind, version: &str) -> Vec<String> {
    if kind == SourceKind::DirectArchive {
        // A fixed URL cannot serve another version: packaging 3.1.4 as 3.2.0
        // would be silently wrong. Only a name without a version is trusted.
        if let Some((_, v)) = archive_name_version(last_segment(url))
            && !same_version(&v, version)
        {
            ap_core::diag!(
                "  {url} is version {v}, not the requested {version}; \
                 a direct archive URL cannot provide another version"
            );
            return Vec::new();
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

/// Get all tags from a repository via the forge API.
fn get_repo_tags(repo_url: &str) -> Result<Vec<String>, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Ok(Vec::new());
    }
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
    let is_gitlab =
        crate::http::host_of(base).is_some_and(|h| h == "gitlab.com" || h.starts_with("gitlab."));

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
            // A tag from the API or a guess may hold characters that are not
            // URL-safe; encode each segment but keep `/` (`release/1.0`).
            let tag = tag
                .split('/')
                .map(|seg| urlencoding::encode(seg).into_owned())
                .collect::<Vec<_>>()
                .join("/");
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
    #[test]
    fn name_normalisation_is_pep_503() {
        for n in ["foo.bar", "foo--bar", "Foo_Bar", "FOO-._bar"] {
            assert_eq!(super::normalize_name(n), "foo-bar", "{n}");
        }
        let mut m = super::SourceMap::default();
        m.insert("Foo_Bar", "https://github.com/o/r");
        assert!(m.get("foo.bar").is_some() && m.get("foo--bar").is_some());
    }

    #[test]
    fn version_from_tag_survives_non_ascii_tags() {
        assert_eq!(super::version_from_tag("réléase-1", "foo"), "réléase-1");
        assert_eq!(super::version_from_tag("fé", "foo"), "fé");
        assert_eq!(super::version_from_tag("foo-v1.2", "foo"), "1.2");
    }

    #[test]
    fn forge_archive_urls_are_forge_not_direct() {
        // A fixed archive file is what the URL names; only repository and tag
        // URLs go through the forge.
        assert_eq!(
            super::source_kind("https://github.com/o/r/archive/v1.0.tar.gz"),
            super::SourceKind::DirectArchive
        );
        assert_eq!(
            super::source_kind("https://github.com/o/r/tree/v1.0"),
            super::SourceKind::Forge
        );
    }

    #[test]
    fn direct_archive_must_match_requested_version() {
        let url = "https://example.org/dl/foo-3.1.4.tar.gz";
        let kind = super::SourceKind::DirectArchive;
        assert!(super::archive_urls_non_forge(url, kind, "3.2.0").is_empty());
        assert_eq!(super::archive_urls_non_forge(url, kind, "3.1.4"), vec![url]);
        let bare = "https://example.org/dl/latest.tar.gz";
        assert_eq!(
            super::archive_urls_non_forge(bare, kind, "3.2.0"),
            vec![bare]
        );
    }

    #[test]
    fn redirects_may_not_downgrade_to_http() {
        let from = "https://api.example.org/x";
        assert!(super::redirect_target(from, "http://evil.example/x").is_err());
        assert!(super::redirect_target(from, "HTTP://evil.example/x").is_err());
        assert_eq!(
            super::redirect_target(from, "https://b.example/y").unwrap(),
            "https://b.example/y"
        );
        assert_eq!(
            super::redirect_target(from, "/z").unwrap(),
            "https://api.example.org/z"
        );
    }

    use super::*;

    #[test]
    fn a_fetched_archive_is_moved_to_its_destination_without_a_download() {
        // The probe's copy is handed over, not fetched again. The URLs are
        // unique to this test and never requested.
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("cached.tar.gz");
        std::fs::write(&cached, b"archive").unwrap();
        let url = "https://example.invalid/zz-handover-1.0.tar.gz";
        archive_cache()
            .lock()
            .unwrap()
            .files
            .insert(url.to_string(), cached.clone());
        let dest = dir.path().join("out.tar.gz");
        fetch_archive_to(url, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"archive");
        assert!(!cached.exists());

        let gone = "https://example.invalid/zz-handover-missing-1.0.tar.gz";
        archive_cache()
            .lock()
            .unwrap()
            .missing
            .insert(gone.to_string());
        assert_eq!(
            fetch_archive_to(gone, &dest).unwrap_err(),
            FetchFailure::NotFound
        );
    }

    /// Tests that touch the process environment must not run concurrently.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Baseline `SdistInfo` with all layout booleans cleared — build it
    /// up per-test by toggling the fields the scenario depends on, so
    /// the test reads as "project X with layout Y" instead of a wall of
    /// `..Default::default()` boilerplate.
    fn bare_info() -> ap_ecosystem::sdist::SdistInfo {
        ap_ecosystem::sdist::SdistInfo {
            project_name: None,
            project_dir: None,
            top_level_dir: None,
            subprojects: Vec::new(),
            build_system: ap_ecosystem::sdist::BuildSystem {
                requires: Vec::new(),
                backend: None,
            },
            python_packaging: false,
            cargo_manifest: false,
            cmake_manifest: false,
            autotools_manifest: false,
            cmake_at_root: false,
            autotools_at_root: false,
            has_c_extensions: false,
            has_tests: false,
            top_level_tests_dir: None,
            files: Vec::new(),
        }
    }

    /// The llama.cpp motivator: `CMakeLists.txt` + `pyproject.toml` at
    /// the root, pyproject declares `llama-cpp-scripts`, user asked for
    /// `llama-cpp`. Must flip the ecosystem choice to C/C++ instead of
    /// silently renaming through to the Python stub.
    #[test]
    fn python_hijack_fires_when_cmake_and_a_different_pyproject_name_coexist() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.cmake_at_root = true;
        info.project_name = Some("llama-cpp-scripts".to_string());
        assert!(
            is_python_hijack(&info, "llama-cpp"),
            "CMake + pyproject with a different declared name must be \
             treated as a C/C++ project with a Python-helper sidecar"
        );
    }

    /// Same decision on autotools, so the fix is not CMake-only.
    /// google/re2: CMake at the root, a Python wrapper under `python/` whose
    /// setup.py declares no pyproject name. The C++ library is the project.
    #[test]
    fn python_hijack_fires_for_a_nameless_python_binding_in_a_subdirectory() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.cmake_at_root = true;
        info.project_dir = Some("python".to_string());
        assert!(is_python_hijack(&info, "re2"));
        // ...but nameless Python packaging AT the root stays Python.
        info.project_dir = None;
        assert!(!is_python_hijack(&info, "re2"));
    }

    #[test]
    fn python_hijack_also_fires_on_autotools_plus_pyproject() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.autotools_at_root = true;
        info.project_name = Some("some-tool-scripts".to_string());
        assert!(is_python_hijack(&info, "some-tool"));
    }

    /// A maturin project (pyproject + Cargo.toml, no CMake) must NOT
    /// be flipped — it is a Python package that compiles Rust, and the
    /// Python side is primary. This is the invariant the original
    /// comment at `probe` was protecting; the fix must preserve it.
    #[test]
    fn python_hijack_does_not_fire_on_maturin_projects() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.cargo_manifest = true;
        info.cmake_at_root = true;
        info.project_name = Some("totally-different-name".to_string());
        assert!(
            !is_python_hijack(&info, "pydantic-core"),
            "a maturin project (pyproject + Cargo + maybe CMake) must \
             stay Python-primary even when its declared name differs"
        );
    }

    /// A Python-only project (no CMake, no autotools) must still rename
    /// through its declared name — the Flask / Pallets case the original
    /// comment at `probe` was built for.
    #[test]
    fn python_hijack_does_not_fire_without_a_native_manifest() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.project_name = Some("Flask".to_string());
        assert!(!is_python_hijack(&info, "flask"));
    }

    /// If the user asked for the Python name itself (after
    /// normalization), there is no mismatch and nothing to flip —
    /// building `llama-cpp-scripts` on purpose must stay on the Python
    /// path.
    #[test]
    fn python_hijack_does_not_fire_when_the_user_wanted_the_python_name() {
        let mut info = bare_info();
        info.python_packaging = true;
        info.cmake_at_root = true;
        info.project_name = Some("llama-cpp-scripts".to_string());
        assert!(!is_python_hijack(&info, "llama_cpp_scripts"));
        assert!(!is_python_hijack(&info, "Llama-CPP-Scripts"));
    }

    #[test]
    fn source_kind_tells_forges_indexes_and_archives_apart() {
        use SourceKind::*;
        assert_eq!(source_kind("https://github.com/oatpp/oatpp"), Forge);
        assert_eq!(source_kind("https://gitlab.gnome.org/GNOME/glib"), Forge);
        assert_eq!(source_kind("https://git.example.com/owner/repo"), Forge);
        assert_eq!(
            source_kind("https://download.savannah.gnu.org/releases/lzip/"),
            ReleaseIndex
        );
        assert_eq!(source_kind("https://ftp.gnu.org/gnu/hello"), ReleaseIndex);
        assert_eq!(
            source_kind("https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz"),
            DirectArchive
        );
        assert_eq!(
            source_kind("https://example.org/dl/foo-1.0.tar.xz?x=1"),
            DirectArchive
        );
    }

    #[test]
    fn fixed_release_assets_are_direct_archives_but_tag_urls_stay_forge() {
        use SourceKind::*;
        assert_eq!(
            source_kind("https://github.com/o/r/releases/download/v1/foo-1.tar.gz"),
            DirectArchive
        );
        assert_eq!(
            source_kind("https://github.com/o/r/archive/refs/tags/v1.0.tar.gz"),
            DirectArchive
        );
        assert_eq!(source_kind("https://github.com/o/r/releases/tag/v1"), Forge);
        assert_eq!(source_kind("https://github.com/o/r"), Forge);
    }

    #[test]
    fn versions_compare_ignoring_v_prefix_and_trailing_zero_components() {
        assert!(same_version("1.0", "1.0.0"));
        assert!(same_version("v1.0", "1.0"));
        assert!(same_version("1.2.3", "1.2.3"));
        assert!(!same_version("1.0", "1.0.1"));
        assert!(!same_version("1.10", "1.1"));
        assert_eq!(
            archive_urls_non_forge(
                "https://x.org/foo-1.0.tar.gz",
                SourceKind::DirectArchive,
                "1.0.0"
            ),
            vec!["https://x.org/foo-1.0.tar.gz".to_string()]
        );
    }

    #[test]
    fn double_separator_names_keep_matching_their_stored_entry() {
        let m: SourceMap = toml::from_str("[source]\nfoo__bar = \"https://x.org/a\"\n").unwrap();
        assert_eq!(m.get("foo--bar"), Some("https://x.org/a"));
        assert_eq!(m.get("foo__bar"), Some("https://x.org/a"));
        let mut m = m;
        m.insert("foo--bar", "https://x.org/b");
        assert_eq!(m.urls.len(), 1);
        // A name with no stored entry still uses the collapsed key.
        m.insert("baz..qux", "https://x.org/c");
        assert_eq!(m.get("Baz-Qux"), Some("https://x.org/c"));
    }

    #[test]
    fn archive_name_and_version_are_split_at_the_last_dash_before_a_digit() {
        let nv = |f| archive_name_version(f);
        assert_eq!(
            nv("hello-2.12.tar.gz"),
            Some(("hello".into(), "2.12".into()))
        );
        assert_eq!(
            nv("foo-bar-2.0.1.tgz"),
            Some(("foo-bar".into(), "2.0.1".into()))
        );
        assert_eq!(
            nv("lib_x-1.0rc1.tar.gz"),
            Some(("lib_x".into(), "1.0rc1".into()))
        );
        assert_eq!(nv("README"), None);
        assert_eq!(nv("noversion.tar.gz"), None);
        assert_eq!(nv("-1.0.tar.gz"), None);
    }

    /// The positive half: the right host over https does get its token, and a
    /// self-hosted GitLab only when `GITLAB_HOST` names it.
    #[test]
    fn forge_tokens_are_attached_to_the_matching_host_only() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "GITLAB_TOKEN",
            "CI_JOB_TOKEN",
            "GITLAB_HOST",
        ]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
        unsafe {
            for (k, _) in &saved {
                std::env::remove_var(k);
            }
            std::env::set_var("GITHUB_TOKEN", "gh-secret");
            std::env::set_var("GITLAB_TOKEN", "gl-secret");
        }
        let gh = forge_auth_header("https://api.github.com/repos/o/r/tags");
        let gl = forge_auth_header("https://gitlab.com/api/v4/projects?search=x");
        let selfhosted_before = forge_auth_header("https://git.example.org/api/v4/projects");
        unsafe { std::env::set_var("GITLAB_HOST", "git.example.org") };
        let selfhosted_after = forge_auth_header("https://git.example.org/api/v4/projects");
        let other = forge_auth_header("https://evil.example.net/api/v4/projects");
        unsafe {
            for (k, v) in &saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        assert_eq!(gh, Some(("Authorization", "Bearer gh-secret".to_string())));
        assert_eq!(gl, Some(("PRIVATE-TOKEN", "gl-secret".to_string())));
        assert_eq!(selfhosted_before, None);
        assert_eq!(
            selfhosted_after,
            Some(("PRIVATE-TOKEN", "gl-secret".to_string()))
        );
        assert_eq!(other, None);
    }

    #[test]
    fn forge_tokens_go_to_exact_hosts_over_https_only() {
        // SAFETY of test env: variables are process-global; use unlikely values
        // and only assert the negative cases plus host routing.
        assert_eq!(forge_auth_header("https://evil.com/api.github.com/x"), None);
        assert_eq!(
            forge_auth_header("https://gitlab-mirror.evil.com/a/b"),
            None
        );
        assert_eq!(forge_auth_header("http://api.github.com/x"), None);
        assert_eq!(forge_auth_header("https://notgitlab.com/api/v4/x"), None);
    }

    #[test]
    fn index_links_keep_only_file_names() {
        let html = r##"<a href="?C=N;O=D">Name</a> <a href="../">Parent</a>
            <a href='/releases/lzip/lzip-1.24.tar.gz'>x</a>
            <A HREF="lzip-1.24.tar.gz.sig">sig</A>
            <a href="lzip-1.24.tar.gz">dup</a><a href="#top">t</a>"##;
        assert_eq!(
            parse_index_links(html),
            ["lzip-1.24.tar.gz", "lzip-1.24.tar.gz.sig"]
        );
    }

    #[test]
    fn index_links_of_a_large_listing_parse_quickly() {
        let html: String = (0..20_000)
            .map(|i| format!("<a href=\"pkg-{i}.tar.gz\">pkg-{i}</a>\n"))
            .collect();
        let t = std::time::Instant::now();
        assert_eq!(parse_index_links(&html).len(), 20_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn index_archives_sort_numerically_and_demote_prereleases() {
        let links: Vec<String> = [
            "foo-1.9.tar.gz",
            "foo-1.10.tar.gz",
            "foo-2.0rc1.tar.gz",
            "foo-1.10.tar.xz",
            "foo-1.10.tar.gz.sig",
            "other-9.9.tar.gz",
        ]
        .map(String::from)
        .to_vec();
        let got = index_archives(&links, "foo");
        let versions: Vec<_> = got.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(versions, ["1.10", "1.9", "2.0rc1"]);
        assert_eq!(got[0].0, "foo-1.10.tar.gz");
    }

    #[test]
    fn a_hint_that_matches_nothing_falls_back_to_every_tarball() {
        let links: Vec<String> = ["bar-1.0.tar.gz".to_string()].to_vec();
        assert_eq!(index_archives(&links, "foo").len(), 1);
    }

    #[test]
    fn xz_only_index_is_reported_as_unsupported_not_empty() {
        let links: Vec<String> = ["foo-1.0.tar.xz".to_string()].to_vec();
        assert!(index_archives(&links, "foo").is_empty());
        assert!(index_has_only_unsupported(&links));
        assert!(!index_has_only_unsupported(&["x.html".to_string()]));
    }

    #[test]
    fn direct_archive_resolves_without_any_network() {
        let u = "https://example.org/pub/foo-3.1.4.tar.gz";
        assert_eq!(latest_release(u).unwrap(), "3.1.4");
        assert_eq!(archive_urls(u, "3.1.4"), [u]);
    }
}
