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
/// Uses package name and version (or "latest") as the cache key.
fn cache_path(name: &str, version: Option<&str>) -> std::path::PathBuf {
    let key = match version {
        Some(v) => format!("{}-{}.json", name.to_lowercase(), v),
        None => format!("{}-latest.json", name.to_lowercase()),
    };
    cache_dir().join(key)
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
        None => 24 * 60 * 60,         // 1 day for "latest"
    };

    // Check if cache is fresh
    let age = metadata.modified()
        .ok()?
        .elapsed()
        .ok()?
        .as_secs();

    if age > max_age_secs {
        ap_core::diag!("  cache entry stale (age: {} days), re-resolving", age / 86400);
        return None;
    }

    // Load and parse cache
    let contents = std::fs::read_to_string(&path).ok()?;
    let nodes: Vec<Node> = serde_json::from_str(&contents).ok()?;

    ap_core::diag!("  ✓ loaded resolution from cache ({} packages)", nodes.len());
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
    if let Ok(json) = serde_json::to_string_pretty(nodes) {
        if std::fs::write(&path, json).is_ok() {
            ap_core::diag!("  cached resolution for future use");
        }
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

    let out = Command::new("pip")
        .args([
            "install",
            "--dry-run",
            "--ignore-installed",
            "--quiet",
            "--disable-pip-version-check",
            "--report",
            "-",
            &spec,
        ])
        .output()
        .map_err(Error::Launch)?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        // Detect "package not found" errors
        if stderr.contains("Could not find a version that satisfies the requirement")
            || stderr.contains("No matching distribution found for")
        {
            return Err(Error::NotFound(name.to_string()));
        }

        return Err(Error::Failed { code, stderr });
    }

    let report: Report = serde_json::from_slice(&out.stdout)?;
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
}
