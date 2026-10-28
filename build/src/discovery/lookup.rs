use super::*;

pub(super) static AGENT_ENABLED: AtomicBool = AtomicBool::new(true);

pub(super) static AGENT_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// A run discovers dependencies on the fly, and each one that nothing else
/// can place would otherwise cost an agent call. Cap them per process.
pub(super) const MAX_AGENT_LOOKUPS: usize = 5;

pub(super) const AGENT_LOOKUP_BUDGET_USD: &str = "0.30";

pub(super) const AGENT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(240);

/// Turn the agent fallback on or off (`--no-agent` turns it off: a run that
/// promised never to call the model must not do so from here either).
pub fn set_agent_enabled(enabled: bool) {
    AGENT_ENABLED.store(enabled, Ordering::Relaxed);
}

pub(super) fn agent_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The lock that callers looking up `key` queue on. One per distinct name
/// asked about, so the map stays a few entries per run and is never pruned.
pub(super) fn in_flight_slot(key: &str) -> Arc<Mutex<()>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let slots = SLOTS.get_or_init(Default::default);
    slots
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.to_string())
        .or_default()
        .clone()
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

/// The answer in `text`, as a [`Finding`]. Models wrap the object in prose or a
/// code fence, and the prose may hold braces of its own (`I checked
/// {savannah}.`), so each `{` is tried as the start of a JSON object. Only
/// top-level objects that carry a `url` key count, and the last one wins:
/// agents put the answer at the end of the reply.
pub fn parse_finding(text: &str) -> Option<Finding> {
    let mut found = None;
    let mut from = 0;
    while let Some(off) = text[from..].find('{') {
        let start = from + off;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) => {
                // Continue after this object, so braces inside it are not
                // taken as candidates of their own.
                from = start + stream.byte_offset();
                if value.get("url").is_some()
                    && let Ok(finding) = serde_json::from_value::<Finding>(value)
                {
                    found = Some(finding);
                }
            }
            // Not JSON from this brace: the next one may be.
            _ => from = start + 1,
        }
    }
    found
}

/// Whether a URL answers at all (HEAD, then GET for servers that refuse HEAD).
pub(super) fn reachable(url: &str) -> bool {
    crate::http::head(url).call().is_ok() || crate::http::get(url).call().is_ok()
}

/// Package indexes and release hosts the agent may name, besides the forges that
/// `ap_ecosystem::forge` knows. Subdomains of an entry are accepted too.
pub(crate) const TRUSTED_SOURCE_HOSTS: &[&str] = &[
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

/// Whether sources may be fetched from `host`: a fixed forge host, a trusted
/// index or release host, or a host listed in `AUTO_PACKAGE_SOURCE_HOSTS`
/// (comma separated, for a private mirror or a self-hosted forge such as
/// `gitlab.gnome.org`). Self-hosted `gitlab.<x>` names are not trusted by
/// themselves, since any host can be spelled that way.
pub(super) fn host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if ap_ecosystem::forge::is_fixed_forge_host(&host) {
        return true;
    }
    let extra = extra_source_hosts();
    TRUSTED_SOURCE_HOSTS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|domain| host_is_under(&host, domain))
}

/// The hosts listed in `AUTO_PACKAGE_SOURCE_HOSTS` (comma separated),
/// lowercased, without a trailing dot. Read by every source host check.
pub(crate) fn extra_source_hosts() -> Vec<String> {
    let extra = std::env::var("AUTO_PACKAGE_SOURCE_HOSTS").unwrap_or_default();
    extra
        .split(',')
        .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// `host` is `domain` or one of its subdomains.
pub(crate) fn host_is_under(host: &str, domain: &str) -> bool {
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

pub(super) fn agent_prompt(name: &str, hints: &Hints) -> String {
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
    let key = normalize(name);
    // A name already answered comes straight from the cache, without waiting
    // behind model calls in flight for other names.
    if let Some(hit) = agent_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return hit.clone();
    }
    // Callers asking about the same name queue on that name's slot, so the
    // model is asked once and the rest read the cache; other names proceed in
    // parallel.
    let slot = in_flight_slot(&key);
    let _turn = slot.lock().unwrap_or_else(|e| e.into_inner());
    // Another caller may have answered this name while we were queued.
    if let Some(hit) = agent_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
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
pub(super) const AGENT_LOOKUP_MODELS: &[&str] = &["claude-sonnet-5-5", "claude-opus-5-5"];

/// Whether the CLI's output reports an HTTP 429 (quota / rate limit).
pub fn is_rate_limited(stdout: &str) -> bool {
    serde_json::from_str::<Value>(stdout).ok().is_some_and(|v| {
        v.get("api_error_status").and_then(Value::as_u64) == Some(429)
            || v.get("result")
                .and_then(Value::as_str)
                .is_some_and(|r| r.contains("(429)") || r.contains("Quota exceeded"))
    })
}

pub(super) fn cache_agent_answer(key: &str, found: Option<String>) -> Option<String> {
    let mut cache = agent_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(key.to_string(), found.clone());
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
