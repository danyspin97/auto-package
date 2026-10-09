//! The pip dependency resolver.
//!
//! `pip install --dry-run --report -` resolves a package's full runtime
//! closure without installing anything. The report is JSON (PEP 723 report
//! format v1) and contains each node's name, version, sha256, and
//! `requires_dist` with markers unevaluated.
//!
//! We run in default wheel mode. No code from any sdist executes during
//! resolution.
//!
//! Known limitation: `--python-version` is ignored by pip when a real
//! interpreter is running — marker evaluation uses the host interpreter.
//! Cross-flavor resolution (python311 vs python313) requires the actual
//! interpreter binaries to be installed and a separate run per flavor.

use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One package in the resolved closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// Raw `Requires-Dist` lines from the wheel metadata, markers and extras
    /// unevaluated. Empty when the package declares no runtime dependencies.
    pub requires_dist: Vec<String>,
    pub requires_python: Option<String>,
    /// The `requested` flag is true only for the root package — pip sets it to
    /// distinguish what you asked for from what the solver pulled in.
    pub is_requested: bool,
    pub is_yanked: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run pip: {0}")]
    Launch(std::io::Error),

    #[error("package not found on PyPI: {0}")]
    NotFound(String),

    /// Every release exists but pip skipped all of them because their
    /// `Requires-Python` excludes the interpreter pip runs under.
    #[error(
        "{name} is on PyPI, but no release supports the Python that pip runs under \
         (e.g. {example}); put a pip for an older/newer Python first on PATH"
    )]
    IncompatiblePython { name: String, example: String },

    #[error("pip exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("pip produced malformed JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    /// pip resolved the package but couldn't find a sha256 for a node. This
    /// indicates a corrupted or non-standard index, not a usage error.
    #[error("no sha256 for {0}")]
    NoDigest(String),
}

/// Get the cache directory for pip resolution results.
///
/// Caches are stored in `~/.cache/auto-package/pip-resolve/` by default,
/// or `$XDG_CACHE_HOME/auto-package/pip-resolve/` if XDG_CACHE_HOME is set.
fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".cache")
        });
    base.join("auto-package").join("pip-resolve")
}

/// Get the cache file path for a specific package resolution.
///
/// Delegates to [`cache_key`], which also folds in the host interpreter and
/// platform so a cache populated under one Python/arch is not reused under
/// another. The old narrow key (`<name>-<version>.json`) is simply never
/// matched; the existing TTL prunes those stale files.
fn cache_path(name: &str, version: Option<&str>) -> std::path::PathBuf {
    cache_dir().join(cache_key(name, version, &host_python(), &host_platform()))
}

/// Build the cache filename for a resolve result.
///
/// `pip install --dry-run` resolves against the host interpreter and host
/// platform regardless of our `--arch`/`--distro`: `--python-version` is
/// cosmetic once a real interpreter is running, and `pip` has no `--platform`
/// knob that works in the default (non-wheel-download) mode. Two concurrent
/// runs on different Python majors or different arches must not share a
/// cached result, so interpreter + platform go into the key as a short hash.
pub(crate) fn cache_key(name: &str, version: Option<&str>, python: &str, platform: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(python.as_bytes());
    hasher.update([0u8]);
    hasher.update(platform.as_bytes());
    let digest = hex::encode(hasher.finalize());
    let short = &digest[..12];
    let base = match version {
        Some(v) => format!("{}-{}", name.to_lowercase(), v),
        None => format!("{}-latest", name.to_lowercase()),
    };
    format!("{base}-{short}.json")
}

/// Host Python interpreter identifier, from `python3 --version` (e.g.
/// `Python 3.13.1`). Falls back to the empty string if the probe fails — a
/// missing interpreter is a bigger problem than a slightly less specific cache
/// key, and `resolve` itself will fail loudly on the subsequent `pip` call.
fn host_python() -> String {
    Command::new("python3")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            // Older `python3 --version` prints to stderr; newer to stdout.
            let mut out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if out.is_empty() {
                out = String::from_utf8_lossy(&o.stderr).trim().to_string();
            }
            out
        })
        .unwrap_or_default()
}

/// Host platform tag, from `sysconfig.get_platform()` (e.g.
/// `linux-x86_64`). Falls back to `std::env::consts::OS-ARCH` when the Python
/// probe can't run — enough to keep an x86_64 cache from being reused on
/// aarch64 even if `python3` is missing entirely.
fn host_platform() -> String {
    let probed = Command::new("python3")
        .args(["-c", "import sysconfig; print(sysconfig.get_platform())"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    probed.unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

/// Load cached resolution result if available and fresh.
///
/// Cache is considered stale if older than 7 days (for pinned versions) or
/// 1 day (for "latest").
fn load_cached_resolve(name: &str, version: Option<&str>) -> Option<Vec<Node>> {
    let path = cache_path(name, version);

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
        ap_core::diag!(
            "  cache entry stale (age: {} days), re-resolving",
            age / 86400
        );
        return None;
    }

    // Load and parse cache
    let contents = std::fs::read_to_string(&path).ok()?;
    let nodes: Vec<Node> = serde_json::from_str(&contents).ok()?;

    ap_core::diag!(
        "  ✓ loaded resolution from cache ({} packages)",
        nodes.len()
    );
    Some(nodes)
}

/// Save resolution result to cache.
fn save_cached_resolve(name: &str, version: Option<&str>, nodes: &[Node]) {
    let path = cache_path(name, version);

    // Create cache directory if it doesn't exist
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Serialize and save
    if let Ok(json) = serde_json::to_string_pretty(nodes)
        && std::fs::write(&path, json).is_ok()
    {
        ap_core::diag!("  cached resolution for future use");
    }
}

/// Resolve the runtime closure of `name` at `version`.
///
/// `version` may be `None` for "latest". Returns all nodes including the root
/// package itself; callers can filter by `Node::is_requested` if they only
/// want transitive deps.
///
/// Results are cached in `~/.cache/auto-package/pip-resolve/`. Cached results
/// for pinned versions are valid for 7 days, "latest" queries for 1 day.
/// Turn a failed `pip install --dry-run` into the most specific error.
///
/// pip says "No matching distribution found" both when the project does not
/// exist and when every release was skipped for its `Requires-Python`; only
/// the preceding "Ignored the following versions…" line tells them apart.
fn classify_failure(name: &str, code: i32, stderr: String) -> Error {
    if let Some(line) = stderr.lines().find(|l| {
        l.contains("Ignored the following versions that require a different python version")
    }) {
        let example = line
            .split("version: ")
            .nth(1)
            .and_then(|l| l.split("; ").next())
            .unwrap_or("all releases")
            .to_string();
        return Error::IncompatiblePython {
            name: name.to_string(),
            example,
        };
    }
    if stderr.contains("Could not find a version that satisfies the requirement")
        || stderr.contains("No matching distribution found for")
    {
        return Error::NotFound(name.to_string());
    }
    Error::Failed { code, stderr }
}

/// Interpreters tried, in order, when the default pip's Python is outside a
/// project's `Requires-Python`. Stable releases first, newest-compatible
/// guesses before older ones.
const ALTERNATE_PYTHON_MINORS: [u32; 5] = [12, 11, 13, 10, 14];

/// Run `pip install --dry-run --report -` through `program` (a pip command
/// line prefix) and return its JSON report.
fn run_pip_report(program: &[&str], name: &str, spec: &str) -> Result<Vec<u8>, Error> {
    let out = Command::new(program[0])
        .args(&program[1..])
        .args([
            "install",
            "--dry-run",
            "--ignore-installed",
            "--quiet",
            "--disable-pip-version-check",
            "--report",
            "-",
            spec,
        ])
        .output()
        .map_err(Error::Launch)?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(classify_failure(name, code, stderr));
    }
    Ok(out.stdout)
}

pub fn resolve(name: &str, version: Option<&str>) -> Result<Vec<Node>, Error> {
    // Try to load from cache first
    if let Some(cached) = load_cached_resolve(name, version) {
        return Ok(cached);
    }

    ap_core::diag!("  resolving dependencies with pip (not in cache)...");

    let spec = match version {
        Some(v) => format!("{name}=={v}"),
        None => name.to_string(),
    };

    let stdout = match run_pip_report(&["pip"], name, &spec) {
        Err(e @ Error::IncompatiblePython { .. }) => {
            // The default pip's interpreter is outside every release's
            // Requires-Python (open-webui needs <3.13 on a 3.13 system):
            // the project is fine, so resolve under another installed Python.
            let mut found = None;
            for minor in ALTERNATE_PYTHON_MINORS {
                let py = format!("python3.{minor}");
                if let Ok(out) = run_pip_report(&[&py, "-m", "pip"], name, &spec) {
                    ap_core::diag!(
                        "  default pip's Python is incompatible with {name}; resolved with {py}"
                    );
                    found = Some(out);
                    break;
                }
            }
            found.ok_or(e)?
        }
        other => other?,
    };

    let report: Report = serde_json::from_slice(&stdout)?;
    let nodes: Vec<Node> = report
        .install
        .into_iter()
        .map(|e| {
            let sha256 = e
                .download_info
                .archive_info
                .hashes
                .sha256
                .ok_or_else(|| Error::NoDigest(e.metadata.name.clone()))?;
            Ok::<Node, Error>(Node {
                name: e.metadata.name,
                version: e.metadata.version,
                sha256,
                requires_dist: e.metadata.requires_dist,
                requires_python: e.metadata.requires_python,
                is_requested: e.requested,
                is_yanked: e.is_yanked,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    // Save to cache for future use
    save_cached_resolve(name, version, &nodes);

    Ok(nodes)
}

// --- wire format -----------------------------------------------------------

#[derive(Deserialize)]
struct Report {
    install: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    download_info: RawDownloadInfo,
    #[serde(default)]
    is_yanked: bool,
    #[serde(default)]
    requested: bool,
    metadata: RawMetadata,
}

#[derive(Deserialize)]
struct RawDownloadInfo {
    archive_info: RawArchiveInfo,
}

#[derive(Deserialize)]
struct RawArchiveInfo {
    hashes: RawHashes,
}

#[derive(Deserialize, Default)]
struct RawHashes {
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct RawMetadata {
    name: String,
    version: String,
    #[serde(default)]
    requires_dist: Vec<String>,
    #[serde(default)]
    requires_python: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `pip install --dry-run --report -` on httpx 0.28.1.
    /// The wheel URL and long description are dropped; every other field is
    /// verbatim, including the empty `requires_dist` on leaf packages.
    const HTTPX_REPORT: &str = r#"{
      "version": "1",
      "pip_version": "26.2",
      "install": [
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/httpx-0.28.1-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"},
              "hash": "sha256=d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": true,
          "metadata": {
            "metadata_version": "2.3", "name": "httpx", "version": "0.28.1",
            "requires_python": ">=3.8",
            "requires_dist": [
              "anyio", "certifi", "httpcore==1.*", "idna",
              "brotli; (platform_python_implementation == 'CPython') and extra == 'brotli'",
              "click==8.*; extra == 'cli'", "h2<5,>=3; extra == 'http2'"
            ]
          }
        },
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/h11-0.16.0-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"},
              "hash": "sha256=63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": false,
          "metadata": {
            "metadata_version": "2.1", "name": "h11", "version": "0.16.0",
            "requires_python": ">=3.8",
            "requires_dist": []
          }
        },
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/certifi-2026.7.22-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "abc123"},
              "hash": "sha256=abc123"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": false,
          "metadata": {
            "metadata_version": "2.1", "name": "certifi", "version": "2026.7.22"
          }
        }
      ],
      "environment": {
        "python_version": "3.13"
      }
    }"#;

    fn parse(json: &str) -> Vec<Node> {
        let report: Report = serde_json::from_str(json).unwrap();
        report
            .install
            .into_iter()
            .map(|e| {
                let sha256 = e
                    .download_info
                    .archive_info
                    .hashes
                    .sha256
                    .expect("fixture has sha256");
                Node {
                    name: e.metadata.name,
                    version: e.metadata.version,
                    sha256,
                    requires_dist: e.metadata.requires_dist,
                    requires_python: e.metadata.requires_python,
                    is_requested: e.requested,
                    is_yanked: e.is_yanked,
                }
            })
            .collect()
    }

    #[test]
    fn parses_root_and_transitive_nodes() {
        let nodes = parse(HTTPX_REPORT);
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn root_is_marked_requested() {
        let nodes = parse(HTTPX_REPORT);
        let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
        assert!(root.is_requested);
        assert!(!nodes.iter().find(|n| n.name == "h11").unwrap().is_requested);
    }

    #[test]
    fn markers_and_extras_are_kept_unevaluated() {
        let nodes = parse(HTTPX_REPORT);
        let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
        assert!(
            root.requires_dist
                .iter()
                .any(|r| r.contains("extra == 'cli'"))
        );
        assert!(
            root.requires_dist
                .iter()
                .any(|r| r.contains("platform_python_implementation"))
        );
    }

    #[test]
    fn leaf_with_no_requires_dist_field_gives_empty_vec() {
        // `certifi` in the fixture has no `requires_dist` key at all.
        let nodes = parse(HTTPX_REPORT);
        let leaf = nodes.iter().find(|n| n.name == "certifi").unwrap();
        assert!(leaf.requires_dist.is_empty());
    }

    #[test]
    fn sha256_is_extracted_from_hashes_not_the_legacy_hash_field() {
        let nodes = parse(HTTPX_REPORT);
        assert_eq!(
            nodes.iter().find(|n| n.name == "httpx").unwrap().sha256,
            "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
        );
    }

    #[test]
    fn unknown_top_level_fields_do_not_break_parsing() {
        let json = HTTPX_REPORT.replace(
            r#""version": "1""#,
            r#""version": "1", "future_field": {"a": 1}"#,
        );
        assert_eq!(parse(&json).len(), 3);
    }

    #[test]
    fn cache_key_folds_interpreter_and_platform_into_a_short_hash() {
        let key = cache_key("httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        assert!(key.starts_with("httpx-0.28.1-"), "{key}");
        assert!(key.ends_with(".json"), "{key}");
        // 12-char hex hash segment between the version and `.json`.
        let stem = key.strip_suffix(".json").unwrap();
        let hash = stem.rsplit('-').next().unwrap();
        assert_eq!(hash.len(), 12, "{key}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
    }

    #[test]
    fn cache_key_changes_when_interpreter_changes() {
        let a = cache_key("httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        let b = cache_key("httpx", Some("0.28.1"), "Python 3.14.0", "linux-x86_64");
        assert_ne!(a, b, "interpreter must change the cache key");
    }

    #[test]
    fn cache_key_changes_when_platform_changes() {
        let a = cache_key("httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        let b = cache_key("httpx", Some("0.28.1"), "Python 3.13.1", "linux-aarch64");
        assert_ne!(a, b, "platform must change the cache key");
    }

    #[test]
    fn cache_key_is_deterministic_for_the_same_inputs() {
        let a = cache_key("Httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        let b = cache_key("Httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_lowercases_the_package_name() {
        // Avoids `MarkupSafe` vs `markupsafe` producing two different cache
        // entries for the same release.
        let a = cache_key("MarkupSafe", Some("3.0.3"), "p", "l");
        let b = cache_key("markupsafe", Some("3.0.3"), "p", "l");
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_distinguishes_latest_from_pinned() {
        let latest = cache_key("httpx", None, "p", "l");
        let pinned = cache_key("httpx", Some("0.28.1"), "p", "l");
        assert_ne!(latest, pinned);
        assert!(latest.contains("httpx-latest-"), "{latest}");
    }

    #[test]
    fn cache_key_differs_from_the_old_narrow_scheme() {
        // Old key was `<name>-<version>.json`. The new key must never match it,
        // so stale entries from before this change are simply ignored (the TTL
        // in load_cached_resolve prunes them).
        let old = "httpx-0.28.1.json";
        let new = cache_key("httpx", Some("0.28.1"), "Python 3.13.1", "linux-x86_64");
        assert_ne!(old, new);
    }
}

#[cfg(test)]
mod classify_failure_tests {
    use super::*;

    #[test]
    fn skipped_for_requires_python_is_not_reported_as_missing() {
        let stderr = "ERROR: Ignored the following versions that require a different python version: 0.8.12 Requires-Python >=3.11,<3.13.0a1; 0.9.0 Requires-Python >=3.11,<3.13.0a1\nERROR: Could not find a version that satisfies the requirement open-webui (from versions: none)\nERROR: No matching distribution found for open-webui\n";
        match classify_failure("open-webui", 1, stderr.to_string()) {
            Error::IncompatiblePython { name, example } => {
                assert_eq!(name, "open-webui");
                assert_eq!(example, "0.8.12 Requires-Python >=3.11,<3.13.0a1");
            }
            e => panic!("wrong error: {e}"),
        }
    }

    #[test]
    fn a_truly_missing_project_is_not_found() {
        let stderr = "ERROR: Could not find a version that satisfies the requirement nope (from versions: none)\nERROR: No matching distribution found for nope\n";
        assert!(matches!(
            classify_failure("nope", 1, stderr.to_string()),
            Error::NotFound(_)
        ));
    }
}
