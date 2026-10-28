use super::*;

/// Get the cache directory for pip resolution results.
///
/// Caches are stored in `~/.cache/auto-package/pip-resolve/` by default,
/// or `$XDG_CACHE_HOME/auto-package/pip-resolve/` if XDG_CACHE_HOME is set.
/// Only absolute paths are accepted; otherwise a per-user directory under the
/// temp dir is used, so the cache never lands relative to the working directory.
pub(super) fn cache_dir() -> std::path::PathBuf {
    cache_dir_from(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        std::env::temp_dir(),
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() },
    )
}

pub(super) fn cache_dir_from(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    tmp: std::path::PathBuf,
    uid: u32,
) -> std::path::PathBuf {
    let absolute =
        |v: Option<std::ffi::OsString>| v.map(std::path::PathBuf::from).filter(|p| p.is_absolute());
    let base = absolute(xdg)
        .or_else(|| absolute(home).map(|h| h.join(".cache")))
        .unwrap_or_else(|| tmp.join(format!("auto-package-cache-{uid}")));
    base.join("auto-package").join("pip-resolve")
}

/// Get the cache file path for a specific package resolution.
///
/// Delegates to [`cache_key`], which also folds in the host interpreter,
/// platform, resolve mode and the environment pip inherits, so a cache
/// populated under one Python/arch, one mode (sdists allowed or not) or one
/// index is not reused under another. The old narrow key
/// (`<name>-<version>.json`) is simply never matched; the existing TTL prunes
/// those stale files.
///
/// `python` is the version string of the interpreter whose pip produced the
/// closure (see [`resolve`]), not necessarily the host's. `prefix` is that
/// interpreter's prefix when it is known (see [`interpreter_prefix_of`]).
fn cache_path(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
) -> std::path::PathBuf {
    let vars: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    dir.join(cache_key(
        name,
        version,
        python,
        &host_platform(),
        mode,
        &vars,
        prefix,
    ))
}

/// Build the cache filename for a resolve result.
///
/// `pip install --dry-run` resolves against the host interpreter and host
/// platform regardless of our `--arch`/`--distro`: `--python-version` is
/// cosmetic once a real interpreter is running, and `pip` has no `--platform`
/// knob that works in the default (non-wheel-download) mode. Two concurrent
/// runs on different Python majors or different arches must not share a
/// cached result, so interpreter + platform go into the key as a short hash.
///
/// The resolve mode goes in too: a wheels-only closure lacks sdist-only
/// releases and dependencies, and a closure resolved with sdists may contain
/// releases a wheels-only run must reject, so the modes never share an entry.
///
/// `vars` is the environment pip would inherit. Its index, config, CA and
/// proxy variables (see [`RESOLUTION_ENV`]) and the config files pip reads
/// change which closure comes back, so they are folded in too. Only digests
/// go into the hash: no URL, credential or path is stored or printed.
pub(crate) fn cache_key(
    name: &str,
    version: Option<&str>,
    python: &str,
    platform: &str,
    mode: ResolveMode,
    vars: &BTreeMap<OsString, OsString>,
    prefix: Option<&std::path::Path>,
) -> String {
    let configs = config_candidates(vars, prefix);
    cache_key_with_configs(name, version, python, platform, mode, vars, &configs)
}

/// [`cache_key`] over an explicit list of config candidates (see
/// [`config_candidates`]). Split out so tests can inject the system paths.
pub(super) fn cache_key_with_configs(
    name: &str,
    version: Option<&str>,
    python: &str,
    platform: &str,
    mode: ResolveMode,
    vars: &BTreeMap<OsString, OsString>,
    configs: &[Option<std::path::PathBuf>],
) -> String {
    let norm_name = normalize_name(name);
    let mut hasher = Sha256::new();
    hasher.update(python.as_bytes());
    hasher.update([0u8]);
    hasher.update(platform.as_bytes());
    hasher.update([0u8]);
    hasher.update(mode.cache_tag().as_bytes());
    // The raw name/version go into the hash too, so two inputs that
    // sanitize to the same filename part still get distinct keys.
    hasher.update([0u8]);
    hasher.update(norm_name.as_bytes());
    hasher.update([0u8]);
    hasher.update(version.unwrap_or("").as_bytes());
    // Fixed order (the constant's, then the config slots), so the same inputs
    // always hash the same. An unset variable and an empty one differ.
    for var in RESOLUTION_ENV {
        hasher.update(var.as_bytes());
        hasher.update([0u8]);
        let value = vars
            .get(OsStr::new(var))
            .map(|v| sha256(v.as_encoded_bytes()));
        hash_field(&mut hasher, value.as_deref());
    }
    for config in config_digests(configs) {
        hash_field(&mut hasher, config.as_deref());
    }
    let digest = hex::encode(hasher.finalize());
    let short = &digest[..12];
    let base = match version {
        Some(v) => format!("{}-{}", filename_safe(&norm_name), filename_safe(v)),
        None => format!("{}-latest", filename_safe(&norm_name)),
    };
    format!("{base}-{short}.json")
}

/// Environment variables pip inherits that can change what it resolves: the
/// index, extra indexes, trusted hosts and config file, and the CA and proxy
/// settings that decide which server answers. Every name here must also be in
/// [`PIP_ENV_ALLOWLIST`], or pip never sees it.
pub(super) const RESOLUTION_ENV: &[&str] = &[
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PIP_CONFIG_FILE",
    "PIP_CERT",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// Digests of the config files in `candidates` (see [`config_candidates`]), in
/// order. A candidate that is absent or not a regular file is `None`.
fn config_digests(candidates: &[Option<std::path::PathBuf>]) -> Vec<Option<Vec<u8>>> {
    candidates
        .iter()
        .map(|candidate| {
            let file = candidate.as_ref().filter(|p| p.is_file())?;
            std::fs::read(file).ok().map(|bytes| sha256(&bytes))
        })
        .collect()
}

/// SHA-256 of `bytes`, as raw digest bytes.
fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// Feed one optional digest into `hasher`, tagged so `None` and `Some` never
/// produce the same bytes. Digests are fixed-length, so no framing is needed.
fn hash_field(hasher: &mut Sha256, field: Option<&[u8]>) {
    match field {
        Some(bytes) => {
            hasher.update([1u8]);
            hasher.update(bytes);
        }
        None => hasher.update([0u8]),
    }
}

/// Load cached resolution result if available and fresh.
///
/// Cache is considered stale if older than 7 days (for pinned versions) or
/// 1 day (for "latest").
pub(super) fn load_cached_resolve(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
) -> Option<Vec<Node>> {
    let path = cache_path(dir, name, version, python, prefix, mode);

    // Check if cache file exists
    let metadata = std::fs::metadata(&path).ok()?;

    // Determine freshness threshold
    let max_age_secs = match version {
        Some(_) => 7 * 24 * 60 * 60, // 7 days for pinned versions
        None => 24 * 60 * 60,        // 1 day for "latest"
    };

    // Check if cache is fresh
    let age = metadata.modified().ok()?.elapsed().ok()?.as_secs();

    if age > max_age_secs {
        let _ = std::fs::remove_file(&path);
        ap_core::diag!(
            "  cache entry stale (age: {} days), re-resolving",
            age / 86400
        );
        return None;
    }

    // Load and parse cache
    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<Vec<Node>>(&c).ok());
    let Some(nodes) = parsed else {
        // Corrupt or unreadable: drop it so the next run re-resolves cleanly.
        let _ = std::fs::remove_file(&path);
        return None;
    };

    ap_core::diag!(
        "  ✓ loaded resolution from cache ({} packages)",
        nodes.len()
    );
    Some(nodes)
}

/// Save resolution result to cache, keyed by the interpreter that produced it.
pub(super) fn save_cached_resolve(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
    nodes: &[Node],
) {
    let path = cache_path(dir, name, version, python, prefix, mode);

    // Create cache directory if it doesn't exist
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Serialize and save
    // Write to a temp file in the same directory and rename into place, so
    // a concurrent reader never sees a half-written cache file.
    if let Ok(json) = serde_json::to_string_pretty(nodes)
        && ap_core::fsx::write_atomic(&path, json).is_ok()
    {
        ap_core::diag!("  cached resolution for future use");
    }
}
