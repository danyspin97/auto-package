//! Finding an upstream source repository for a package name, beyond GitHub.
//!
//! A bare name that PyPI does not carry (a C/C++ library, a project hosted on
//! GitLab or Codeberg, a distro-only name) used to be looked up on GitHub
//! alone. This module adds the other places an upstream is likely to live:
//!
//! * **The distribution's own spec** (`openSUSE:Factory`, read with `osc cat`)
//!   — its `URL:` and `Source:` tags are what openSUSE already records as the
//!   project's upstream, which settles name collisions (an unrelated
//!   `lzip` binding on GitHub vs. the real one on Savannah) and reveals
//!   non-forge release directories.
//! * **GitLab.com** and **Codeberg** (Gitea API) project search.
//!
//! Every network call is a thin wrapper around a pure parser, so the
//! response handling is tested against fixtures.

use std::collections::HashMap;

use serde_json::Value;

/// Hosts whose URLs are treated as source forges worth trying when they turn
/// up as a homepage in Repology. Anything else (project websites, docs) is
/// ignored: there is no generic way to turn those into a source archive.
const FORGE_HOSTS: &[&str] = &["github.com", "gitlab.com", "codeberg.org", "git.sr.ht"];

/// Whether `url` names a repository (`host/owner/repo`), not just an owner,
/// organisation or user page (`host/owner`). Both answer HTTP 200, so a
/// reachability check alone cannot tell them apart — and a profile page has
/// no tags, releases or archives to package.
pub fn is_repository_url(url: &str) -> bool {
    ap_ecosystem::forge::parse(url)
        .is_some_and(|f| f.project_path.split('/').filter(|s| !s.is_empty()).count() >= 2)
}

fn normalize(name: &str) -> String {
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
fn exact_only(hits: &[(String, String)], pkg: &str) -> Vec<String> {
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
fn spec_tag<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let (k, v) = line.split_once(':')?;
    k.trim()
        .to_ascii_lowercase()
        .starts_with(tag)
        .then(|| v.trim().split('#').next().unwrap_or("").trim())
}

fn expand_name(s: &str, name: &str) -> String {
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
            let url = expand_name(v, name);
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
            let src = expand_name(v, name);
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
    ap_core::diag!("  checking openSUSE:Factory for {name}");
    let spec_file = format!("{name}.spec");
    let Ok(out) = crate::proc::capture_with_timeout(
        "osc",
        &["cat", "openSUSE:Factory", &name, &spec_file],
        &std::env::temp_dir(),
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

fn get_json(url: &str) -> Option<Value> {
    crate::sources::get_json_for_search(url)
        .inspect_err(|e| ap_core::diag!("  search request failed: {e}"))
        .ok()
}

pub fn search_gitlab(pkg: &str) -> Vec<String> {
    let name = normalize(pkg);
    ap_core::diag!("  searching GitLab for: {name}");
    get_json(&format!(
        "https://gitlab.com/api/v4/projects?search={}&order_by=star_count&sort=desc&per_page=10",
        urlencoding::encode(&name)
    ))
    .map(|j| parse_gitlab_search(&j, pkg))
    .unwrap_or_default()
}

pub fn search_codeberg(pkg: &str) -> Vec<String> {
    let name = normalize(pkg);
    ap_core::diag!("  searching Codeberg for: {name}");
    get_json(&format!(
        "https://codeberg.org/api/v1/repos/search?q={}&sort=stars&order=desc&limit=10",
        urlencoding::encode(&name)
    ))
    .map(|j| parse_codeberg_search(&j, pkg))
    .unwrap_or_default()
}

/// Candidates from every non-GitHub source, most trustworthy first (the
/// distro spec's own forge upstream, then GitLab, then Codeberg), without
/// duplicates; plus the distro evidence itself.
pub fn search_other_forges(pkg: &str) -> (Vec<String>, DistroInfo) {
    let distro = distro_info(pkg);
    let mut out: Vec<String> = Vec::new();
    for url in distro
        .forge_repos
        .iter()
        .cloned()
        .chain(search_gitlab(pkg))
        .chain(search_codeberg(pkg))
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

fn same_repo(a: &str, b: &str) -> bool {
    match (ap_ecosystem::forge::parse(a), ap_ecosystem::forge::parse(b)) {
        (Some(x), Some(y)) => {
            x.host.eq_ignore_ascii_case(&y.host)
                && x.project_path.eq_ignore_ascii_case(&y.project_path)
        }
        _ => false,
    }
}

// ── last resort: ask the agent ─────────────────────────────────────────────────

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

static AGENT_ENABLED: AtomicBool = AtomicBool::new(true);
static AGENT_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// A run discovers dependencies on the fly, and each one that nothing else
/// can place would otherwise cost an agent call. Cap them per process.
const MAX_AGENT_LOOKUPS: usize = 5;
const AGENT_LOOKUP_BUDGET_USD: &str = "0.30";
const AGENT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(240);

/// Turn the agent fallback on or off (`--no-agent` turns it off: a run that
/// promised never to call the model must not do so from here either).
pub fn set_agent_enabled(enabled: bool) {
    AGENT_ENABLED.store(enabled, Ordering::Relaxed);
}

fn agent_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Evidence handed to the agent so it checks rather than starts from scratch.
#[derive(Debug, Default, Clone)]
pub struct Hints {
    /// Upstream addresses the distro's spec records for the project.
    pub homepages: Vec<String>,
    /// Candidates the name search produced, which may be wrong.
    pub candidates: Vec<String>,
}

/// What the agent reports back.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub url: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// The first JSON object in `text`, as a [`Finding`]. Models wrap the object
/// in prose or a code fence often enough that the object is located, not
/// assumed to be the whole reply.
pub fn parse_finding(text: &str) -> Option<Finding> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// Whether a URL answers at all (HEAD, then GET for servers that refuse HEAD).
fn reachable(url: &str) -> bool {
    crate::http::head(url).call().is_ok() || crate::http::get(url).call().is_ok()
}

/// Package indexes and release hosts the agent may name, besides the forges that
/// `ap_ecosystem::forge` knows. Subdomains of an entry are accepted too.
const TRUSTED_SOURCE_HOSTS: &[&str] = &[
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "rubygems.org",
    "registry.npmjs.org",
    "metacpan.org",
    "hackage.haskell.org",
    "ftp.gnu.org",
    "download.gnu.org",
    "download.savannah.gnu.org",
    "sourceforge.net",
    "downloads.sourceforge.net",
    "kernel.org",
    "download.kde.org",
    "download.gnome.org",
];

/// Whether sources may be fetched from `host`: a known forge, a trusted index or
/// release host, or a host listed in `AUTO_PACKAGE_SOURCE_HOSTS` (comma
/// separated, for a private mirror).
fn host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if ap_ecosystem::forge::is_known_host(&host) {
        return true;
    }
    let extra = std::env::var("AUTO_PACKAGE_SOURCE_HOSTS").unwrap_or_default();
    TRUSTED_SOURCE_HOSTS
        .iter()
        .copied()
        .chain(extra.split(',').map(str::trim).filter(|h| !h.is_empty()))
        .any(|domain| host_is_under(&host, &domain.to_ascii_lowercase()))
}

/// `host` is `domain` or one of its subdomains.
fn host_is_under(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|h| h.ends_with('.'))
}

/// Accept the agent's URL only if the pipeline can actually use it. The model
/// can name a plausible but wrong or dead URL; nothing it says is trusted
/// until it has been checked the same way a guessed candidate would be.
///
/// Returns the URL to use (an index URL is normalised to end in `/`).
pub fn validate_found_url(url: &str) -> Result<String, String> {
    use crate::sources::{SourceKind, latest_release, source_kind};
    let url = url.trim();
    // https only: the agent's answer is fetched and packaged, and plain http
    // lets anyone on the path swap the source.
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(format!("{url} is not an https URL"));
    }
    // The agent can name any host it finds, and the answer is fetched, so the
    // host must be one the pipeline already trusts to serve sources.
    let host = crate::http::host_of(url).unwrap_or_default();
    if !host_allowed(&host) {
        return Err(format!(
            "{url} is on {host}, which is not a known forge or package host; \
             set AUTO_PACKAGE_SOURCE_HOSTS={host} to allow it"
        ));
    }
    match source_kind(url) {
        SourceKind::Forge => {
            if !is_repository_url(url) {
                return Err(format!(
                    "{url} is an owner/organisation page, not a repository"
                ));
            }
            if !reachable(url) {
                return Err(format!("{url} does not answer"));
            }
            Ok(url.to_string())
        }
        SourceKind::ReleaseIndex => {
            latest_release(url)?;
            Ok(format!("{}/", url.trim_end_matches('/')))
        }
        SourceKind::DirectArchive => {
            latest_release(url)?;
            if !reachable(url) {
                return Err(format!("{url} does not answer"));
            }
            Ok(url.to_string())
        }
    }
}

fn agent_prompt(name: &str, hints: &Hints) -> String {
    let mut evidence = String::new();
    if !hints.homepages.is_empty() {
        evidence.push_str(&format!(
            "\nopenSUSE's own packaging records these upstream addresses for it: {}.\n",
            hints
                .homepages
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !hints.candidates.is_empty() {
        evidence.push_str(&format!(
            "\nA name search found these repositories, which may be DIFFERENT projects that merely share the name (bindings, ports, forks): {}. Do not accept one unless you confirm it is the real upstream.\n",
            hints.candidates.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    format!(
        "Find where the upstream SOURCE CODE of the software called \"{name}\" is released, \
         so it can be packaged for openSUSE. Use WebSearch and WebFetch.\n\
         \n\
         It is not on PyPI and was not found by name on GitHub, GitLab or Codeberg, so it is \
         probably hosted elsewhere: GNU Savannah (download.savannah.gnu.org/releases/<project>/), \
         ftp.gnu.org/gnu/<project>/, SourceForge, sourcehut, a project's own website, or under a \
         different repository name. The name may be a distro package name (a `lib` prefix or \
         `-devel` suffix already stripped), so the project's real name can differ.\n\
         {evidence}\n\
         Answer with the best of these, in order of preference:\n\
         1. a repository on a git forge: https://github.com/<owner>/<repo> (also GitLab, Codeberg), \
            the real repository and not an owner/organisation page;\n\
         2. a release DIRECTORY URL whose HTML listing links the release tarballs, for example \
            https://download.savannah.gnu.org/releases/<project>/ — the tarballs must include \
            .tar.gz files;\n\
         3. a direct URL to one .tar.gz release tarball.\n\
         \n\
         Open the URL you propose and confirm it exists and holds releases; never invent or guess \
         a URL. If you cannot find a trustworthy upstream, say so.\n\
         \n\
         Reply with ONLY one JSON object, no other text: \
         {{\"url\": \"<the URL>\" or null, \"reason\": \"<one sentence: what it is and how you verified it>\"}}"
    )
}

/// Ask the agent to locate an upstream for `name` when every deterministic
/// source has come up empty. Returns a validated URL, or `None`.
///
/// Bounded three ways: only when enabled, at most [`MAX_AGENT_LOOKUPS`] per
/// process, and each lookup is capped by a small budget and a timeout. A
/// name is only ever looked up once (negative results are cached too).
pub fn agent_find_source(name: &str, hints: &Hints) -> Option<String> {
    if !AGENT_ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    // One lookup at a time: with the cache check and the insert separated by a
    // slow model call, concurrent callers asking about the same name each
    // spawned the CLI and each spent one of the per-process lookups.
    static LOOKUP_LOCK: Mutex<()> = Mutex::new(());
    let _one_at_a_time = LOOKUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key = normalize(name);
    if let Some(hit) = agent_cache().lock().ok()?.get(&key) {
        return hit.clone();
    }
    if AGENT_LOOKUPS.fetch_add(1, Ordering::Relaxed) >= MAX_AGENT_LOOKUPS {
        ap_core::diag!("  agent lookup limit reached; not searching for {name}");
        return None;
    }

    ap_core::diag!("  no source found by search — asking the agent to look for {name}");
    // Quotas are per model: when one answers 429, the next may still have room.
    let mut found = None;
    // A private, empty working directory: the CLI reads project settings from
    // its cwd, and the shared temp directory could hold a planted file.
    let Ok(cwd) = tempfile::tempdir() else {
        return None;
    };
    // Only a model actually answering is definitive. A rate limit, timeout or
    // spawn failure says nothing about the package, so it must not be cached
    // as "no source" for the rest of the process.
    let mut answered = false;
    for model in AGENT_LOOKUP_MODELS {
        let out = match crate::package_notes::claude_print_json(
            model,
            "WebSearch,WebFetch",
            AGENT_LOOKUP_BUDGET_USD,
            &agent_prompt(name, hints),
            cwd.path(),
            AGENT_LOOKUP_TIMEOUT,
        ) {
            Ok(o) => o,
            Err(e) => {
                ap_core::diag!("  ✗ agent lookup failed to run ({model}): {e}");
                continue;
            }
        };
        if is_rate_limited(&out.stdout) {
            ap_core::diag!("  agent lookup rate-limited on {model}; trying the next model");
            continue;
        }
        found = interpret_agent_output(&out.stdout, out.code, &out.stderr);
        answered = true;
        break;
    }
    if !answered {
        return found;
    }
    cache_agent_answer(&key, found)
}

/// Models tried in order for a lookup.
const AGENT_LOOKUP_MODELS: &[&str] = &["claude-sonnet-5-5", "claude-opus-5-5"];

/// Whether the CLI's output reports an HTTP 429 (quota / rate limit).
pub fn is_rate_limited(stdout: &str) -> bool {
    serde_json::from_str::<Value>(stdout).ok().is_some_and(|v| {
        v.get("api_error_status").and_then(Value::as_u64) == Some(429)
            || v.get("result")
                .and_then(Value::as_str)
                .is_some_and(|r| r.contains("(429)") || r.contains("Quota exceeded"))
    })
}

fn cache_agent_answer(key: &str, found: Option<String>) -> Option<String> {
    if let Ok(mut cache) = agent_cache().lock() {
        cache.insert(key.to_string(), found.clone());
    }
    found
}

/// Turn the agent CLI's `--output-format json` stdout into a validated URL,
/// saying why whenever it cannot. Separate from the process spawn so each
/// failure shape is testable.
pub fn interpret_agent_output(stdout: &str, code: i32, stderr: &str) -> Option<String> {
    let head = |s: &str| s.chars().take(300).collect::<String>().replace('\n', " ");
    let Ok(v) = serde_json::from_str::<Value>(stdout) else {
        ap_core::diag!(
            "  ✗ agent lookup: exit {code}, output is not JSON: {}",
            head(if stdout.trim().is_empty() {
                stderr
            } else {
                stdout
            })
        );
        return None;
    };
    if v.get("is_error").and_then(Value::as_bool) == Some(true) || code != 0 {
        ap_core::diag!(
            "  ✗ agent lookup errored (exit {code}, subtype {}): {}",
            v.get("subtype").and_then(Value::as_str).unwrap_or("?"),
            head(v.get("result").and_then(Value::as_str).unwrap_or(stderr))
        );
        return None;
    }
    let Some(text) = v.get("result").and_then(Value::as_str) else {
        ap_core::diag!("  ✗ agent lookup: no `result` in its JSON output");
        return None;
    };
    let Some(finding) = parse_finding(text) else {
        ap_core::diag!(
            "  ✗ agent lookup: reply held no JSON answer: {}",
            head(text)
        );
        return None;
    };
    let Some(url) = finding.url else {
        ap_core::diag!(
            "  ✗ agent found no upstream{}",
            finding.reason.map(|r| format!(": {r}")).unwrap_or_default()
        );
        return None;
    };
    match validate_found_url(&url) {
        Ok(u) => {
            ap_core::diag!(
                "  ✓ agent found {u}{}",
                finding
                    .reason
                    .map(|r| format!(" — {r}"))
                    .unwrap_or_default()
            );
            Some(u)
        }
        Err(e) => {
            ap_core::diag!("  ✗ agent's answer rejected: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalize_is_pep_503() {
        for n in ["foo.bar", "foo--bar", "Foo_Bar"] {
            assert_eq!(super::normalize(n), "foo-bar", "{n}");
        }
    }

    #[test]
    fn found_url_must_be_https() {
        let e = super::validate_found_url("http://github.com/o/r").unwrap_err();
        assert!(e.contains("https"), "{e}");
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn org_and_user_pages_are_not_repositories() {
        assert!(!is_repository_url("https://github.com/oatpp"));
        assert!(!is_repository_url("https://github.com/cuda"));
        assert!(!is_repository_url("https://codeberg.org/someone/"));
        assert!(is_repository_url("https://github.com/oatpp/oatpp"));
        assert!(is_repository_url("https://gitlab.gnome.org/GNOME/glib"));
        assert!(is_repository_url(
            "https://gitlab.com/group/subgroup/project"
        ));
        assert!(!is_repository_url("not a url"));
        assert!(!is_repository_url(""));
    }

    fn hits(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, u)| (n.to_string(), u.to_string()))
            .collect()
    }

    #[test]
    fn exact_matches_rank_before_close_ones_and_unrelated_are_dropped() {
        let h = hits(&[
            ("foo-cpp", "u/foo-cpp"),
            ("LeetCUDA", "u/leet"),
            ("foo", "u/foo-1"),
            ("foo", "u/foo-2"),
            ("cpp-foo", "u/cpp-foo"),
            ("foobar", "u/foobar"),
        ]);
        assert_eq!(
            rank_name_matches(&h, "foo"),
            ["u/foo-1", "u/foo-2", "u/foo-cpp", "u/cpp-foo"]
        );
    }

    #[test]
    fn name_matching_ignores_case_and_separators() {
        let h = hits(&[("Yaml_Cpp", "u/y"), ("yamlcpp", "u/z")]);
        assert_eq!(rank_name_matches(&h, "yaml-cpp"), ["u/y", "u/z"]);
    }

    #[test]
    fn source_hosts_are_matched_by_domain() {
        assert!(host_allowed("github.com"));
        assert!(host_allowed("gitlab.gnome.org"));
        assert!(host_allowed("pypi.org"));
        assert!(host_allowed("test.pypi.org"));
        assert!(!host_allowed("example.com"));
        assert!(!host_allowed("pypi.org.example.com"));
        assert!(!host_allowed("notpypi.org"));
    }

    #[test]
    fn an_unknown_host_is_refused_before_any_request() {
        let err = validate_found_url("https://example.com/pkg-1.0.tar.gz").unwrap_err();
        assert!(
            err.contains("AUTO_PACKAGE_SOURCE_HOSTS=example.com"),
            "{err}"
        );
    }

    #[test]
    fn parses_gitlab_and_codeberg_search_responses() {
        let gl = json!([
            {"path": "libfoo", "web_url": "https://gitlab.com/a/libfoo"},
            {"path": "foo", "web_url": "https://gitlab.com/b/foo"},
            {"path": "missing-url"}
        ]);
        assert_eq!(
            parse_gitlab_search(&gl, "foo"),
            ["https://gitlab.com/b/foo"]
        );
        let cb = json!({"ok": true, "data": [
            {"name": "foo", "html_url": "https://codeberg.org/c/foo"},
            {"name": "other", "html_url": "https://codeberg.org/c/other"}
        ]});
        assert_eq!(
            parse_codeberg_search(&cb, "foo"),
            ["https://codeberg.org/c/foo"]
        );
        assert!(parse_gitlab_search(&json!({"message": "rate limited"}), "foo").is_empty());
        assert!(parse_codeberg_search(&json!([]), "foo").is_empty());
    }

    const LZIP_SPEC: &str = "Name:           lzip
URL:            https://www.nongnu.org/lzip/lzip.html
Source:         https://download.savannah.gnu.org/releases/lzip/%name-%version.tar.gz
Source2:        https://download.savannah.gnu.org/releases/lzip/%name-%version.tar.gz.sig
Source3:        %name.keyring
";

    #[test]
    fn spec_with_a_savannah_source_yields_a_release_directory() {
        let i = parse_spec_upstream(LZIP_SPEC, "lzip");
        assert!(i.forge_repos.is_empty());
        assert_eq!(
            i.index_candidates,
            ["https://download.savannah.gnu.org/releases/lzip/"]
        );
        assert!(
            i.homepages
                .contains(&"https://www.nongnu.org/lzip/lzip.html".to_string())
        );
    }

    #[test]
    fn spec_with_forge_url_and_archive_source_yields_the_repository() {
        let spec = "URL: https://github.com/oatpp/oatpp/\nSource0: https://github.com/oatpp/oatpp/archive/%{version}.tar.gz#/%{name}-%{version}.tar.gz\n";
        let i = parse_spec_upstream(spec, "oatpp");
        assert_eq!(i.forge_repos, ["https://github.com/oatpp/oatpp"]);
        assert!(i.index_candidates.is_empty());
    }

    #[test]
    fn spec_addresses_with_unexpanded_macros_or_foreign_schemes_are_dropped() {
        let spec = "URL: https://%{host}/x\nSource0: https://example.org/%{subdir}/foo-1.tar.gz\nSource1: ftp://example.org/foo-1.tar.gz\nSource2: foo.tar.gz\nSource3: https://example.org/foo-1.tar.xz\n";
        assert_eq!(parse_spec_upstream(spec, "foo"), DistroInfo::default());
        assert_eq!(parse_spec_upstream("", "foo"), DistroInfo::default());
    }

    #[test]
    fn unconfirmed_when_the_distro_records_an_upstream_no_candidate_matches() {
        let none = DistroInfo::default();
        let c = vec!["https://github.com/neuromorphicsystems/lzip".to_string()];
        assert!(
            !candidates_unconfirmed(&c, &none),
            "distro silent: nothing to contradict"
        );

        let off_forge = DistroInfo {
            forge_repos: vec![],
            homepages: vec!["https://www.nongnu.org/lzip".into()],
            index_candidates: vec![],
        };
        assert!(candidates_unconfirmed(&c, &off_forge));

        let confirmed = DistroInfo {
            forge_repos: vec!["https://github.com/NeuromorphicSystems/lzip".into()],
            homepages: vec!["https://github.com/NeuromorphicSystems/lzip".into()],
            index_candidates: vec![],
        };
        assert!(
            !candidates_unconfirmed(&c, &confirmed),
            "case-insensitive match"
        );
        assert!(candidates_unconfirmed(&[], &off_forge));

        let with_index = DistroInfo {
            forge_repos: vec![],
            homepages: vec!["https://download.savannah.gnu.org/releases/lzip/".into()],
            index_candidates: vec!["https://download.savannah.gnu.org/releases/lzip/".into()],
        };
        let c = vec!["https://download.savannah.gnu.org/releases/lzip".to_string()];
        assert!(
            !candidates_unconfirmed(&c, &with_index),
            "a confirmed index counts"
        );
    }

    #[test]
    fn secondary_forges_keep_exact_names_only() {
        let gl = json!([
            {"path": "lzip-go", "web_url": "https://gitlab.com/a/lzip-go"},
            {"path": "lzip", "web_url": "https://gitlab.com/b/lzip"}
        ]);
        assert_eq!(
            parse_gitlab_search(&gl, "lzip"),
            ["https://gitlab.com/b/lzip"]
        );
    }

    #[test]
    fn finding_is_found_inside_prose_or_a_code_fence() {
        let f = parse_finding(
            "Sure!\n```json\n{\"url\": \"https://ftp.gnu.org/gnu/hello/\", \"reason\": \"listing\"}\n```",
        )
        .unwrap();
        assert_eq!(f.url.as_deref(), Some("https://ftp.gnu.org/gnu/hello/"));
        assert_eq!(parse_finding("{\"url\": null}").unwrap().url, None);
        assert!(parse_finding("no json here").is_none());
        assert!(parse_finding("{not json}").is_none());
    }

    #[test]
    fn validation_rejects_non_http_and_org_pages_without_network() {
        assert!(validate_found_url("ftp://ftp.gnu.org/gnu/hello/").is_err());
        assert!(validate_found_url("javascript:alert(1)").is_err());
        assert!(validate_found_url("https://github.com/someorg").is_err());
        assert!(validate_found_url("").is_err());
    }

    #[test]
    fn agent_output_failure_shapes_all_yield_none_without_panicking() {
        assert_eq!(interpret_agent_output("", 1, "boom"), None);
        assert_eq!(interpret_agent_output("not json", 0, ""), None);
        assert_eq!(
            interpret_agent_output(
                r#"{"is_error":true,"subtype":"error_max_budget_usd","result":"x"}"#,
                1,
                ""
            ),
            None
        );
        assert_eq!(interpret_agent_output(r#"{"foo":1}"#, 0, ""), None);
        assert_eq!(
            interpret_agent_output(r#"{"result":"nothing here"}"#, 0, ""),
            None
        );
        assert_eq!(
            interpret_agent_output(
                r#"{"result":"{\"url\": null, \"reason\": \"unknown\"}"}"#,
                0,
                ""
            ),
            None
        );
        // A syntactically fine answer with an unusable URL is rejected.
        assert_eq!(
            interpret_agent_output(r#"{"result":"{\"url\": \"ftp://x/y\"}"}"#, 0, ""),
            None
        );
    }

    #[test]
    fn rate_limit_replies_are_recognised() {
        assert!(is_rate_limited(
            r#"{"is_error":true,"result":"API Error: Request rejected (429) · quota"}"#
        ));
        assert!(is_rate_limited(r#"{"api_error_status":429,"result":""}"#));
        assert!(!is_rate_limited(r#"{"result":"{\"url\": null}"}"#));
        assert!(!is_rate_limited("not json"));
    }

    #[test]
    fn agent_lookup_is_off_when_disabled() {
        set_agent_enabled(false);
        assert_eq!(
            agent_find_source("anything-at-all", &Hints::default()),
            None
        );
        set_agent_enabled(true);
    }

    #[test]
    fn prompt_names_the_project_and_demands_json_only() {
        let p = agent_prompt("lzip", &Hints::default());
        assert!(p.contains("\"lzip\""));
        assert!(p.contains("savannah"));
        assert!(p.contains("ONLY one JSON object"));
    }
}
