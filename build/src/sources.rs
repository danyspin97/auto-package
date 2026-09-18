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
        let text = toml::to_string_pretty(self)
            .map_err(|e| format!("serializing source map: {e}"))?;
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
}

impl Ecosystem {
    /// The openSUSE package name for `pkg` built under this ecosystem.
    pub fn rpm_name(self, pkg: &str) -> String {
        match self {
            Ecosystem::Python => format!("python-{}", pkg.to_lowercase()),
            Ecosystem::Rust => pkg.to_lowercase(),
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
        let Ok(resp) = ureq::get(url).call() else { continue };
        let mut buf = Vec::new();
        use std::io::Read;
        if resp.into_body().into_reader().read_to_end(&mut buf).is_err() || buf.is_empty() {
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
        } else {
            return Err(format!(
                "{url} contains no pyproject.toml, setup.py or Cargo.toml, so \
                 auto-package cannot tell how to build it"
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

    Err(match last_err {
        Some(e) => format!("could not read a release archive for {version}: {e}"),
        None => format!(
            "no release archive for version {version} at {repo_url}. Tried:\n{}",
            candidates
                .iter()
                .map(|u| format!("  {u}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    })
}

/// The host, owner and repository name in a forge URL.
///
/// Both spellings a user is likely to paste are accepted:
/// `https://github.com/NVIDIA/cuda-python(.git)` and
/// `git@github.com:NVIDIA/cuda-python.git`.
fn owner_repo(repo_url: &str) -> Result<(String, String, String), String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    let rest = match trimmed.split_once("://") {
        Some((_, rest)) => rest.to_string(),
        // scp-style: git@host:owner/repo
        None => trimmed
            .split_once('@')
            .map(|(_, hostpath)| hostpath.replacen(':', "/", 1))
            .ok_or_else(|| format!("{repo_url} is not a repository URL"))?,
    };

    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or_default().to_string();
    let owner = parts.next().unwrap_or_default().to_string();
    let repo = parts.next().unwrap_or_default().to_string();
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return Err(format!(
            "cannot tell the owner and repository apart in {repo_url}"
        ));
    }
    Ok((host, owner, repo))
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let body = ureq::get(url)
        .header("User-Agent", "auto-package")
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
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
    let (host, owner, repo) = owner_repo(repo_url)?;

    // Each forge family gets its release endpoint and its tag-list fallback.
    let endpoints: [String; 2] = if host.contains("github.com") {
        [
            format!("https://api.github.com/repos/{owner}/{repo}/releases/latest"),
            format!("https://api.github.com/repos/{owner}/{repo}/tags?per_page=1"),
        ]
    } else if host.contains("gitlab") {
        let project = format!("{owner}%2F{repo}");
        [
            format!("https://{host}/api/v4/projects/{project}/releases?per_page=1"),
            format!("https://{host}/api/v4/projects/{project}/repository/tags?per_page=1"),
        ]
    } else {
        // Gitea and Forgejo, which is what most self-hosted forges are.
        [
            format!("https://{host}/api/v1/repos/{owner}/{repo}/releases/latest"),
            format!("https://{host}/api/v1/repos/{owner}/{repo}/tags?limit=1"),
        ]
    };

    let mut last_err = None;
    for url in &endpoints {
        match get_json(url) {
            Ok(v) => match first_tag(&v) {
                Some(tag) => return Ok(version_from_tag(&tag, &repo)),
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

/// Candidate archive URLs for a release tag, best first.
///
/// Forges disagree on the path: GitHub and the Gitea/Forgejo family serve
/// `/archive/refs/tags/<tag>.tar.gz`, GitLab serves
/// `/-/archive/<tag>/<repo>-<tag>.tar.gz`. Tag spelling is a free-for-all, so
/// several are tried; the first that downloads wins.
pub fn archive_urls(repo_url: &str, version: &str) -> Vec<String> {
    let base = repo_url.trim_end_matches('/').trim_end_matches(".git");
    let repo = base.rsplit('/').next().unwrap_or("source");
    let is_gitlab = base.contains("gitlab");

    let tags = [
        format!("v{version}"),
        version.to_string(),
        format!("{repo}-{version}"),
        format!("{repo}-v{version}"),
        format!("release-{version}"),
        format!("release/{version}"),
    ];

    tags.iter()
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
        let (pkg, url) = SourceMap::parse_arg("cuda-python=https://github.com/x/y", "root").unwrap();
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
        assert!(urls[0].contains("/-/archive/v1.2.3/thing-v1.2.3.tar.gz"), "{urls:?}");

        let urls = archive_urls("https://github.com/owner/thing.git", "1.2.3");
        assert_eq!(urls[0], "https://github.com/owner/thing/archive/refs/tags/v1.2.3.tar.gz");
    }

    #[test]
    fn both_url_spellings_split_the_same_way() {
        for url in [
            "https://github.com/NVIDIA/cuda-python",
            "https://github.com/NVIDIA/cuda-python.git",
            "https://github.com/NVIDIA/cuda-python/",
            "git@github.com:NVIDIA/cuda-python.git",
        ] {
            let (host, owner, repo) = owner_repo(url).unwrap();
            assert_eq!((host.as_str(), owner.as_str(), repo.as_str()),
                       ("github.com", "NVIDIA", "cuda-python"), "{url}");
        }
        assert!(owner_repo("https://github.com/NVIDIA").is_err());
    }

    #[test]
    fn a_tag_is_reduced_to_the_version_it_carries() {
        assert_eq!(version_from_tag("v12.9.0", "cuda-python"), "12.9.0");
        assert_eq!(version_from_tag("12.9.0", "cuda-python"), "12.9.0");
        assert_eq!(version_from_tag("cuda-python-v12.9.0", "cuda-python"), "12.9.0");
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
        assert_eq!(Ecosystem::Python.rpm_name("MarkupSafe"), "python-markupsafe");
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SourceMap::default();
        m.insert("cuda-python", "https://github.com/NVIDIA/cuda-python");
        m.save(dir.path()).unwrap();
        let back = SourceMap::load(dir.path()).unwrap();
        assert_eq!(back.get("cuda_python"), Some("https://github.com/NVIDIA/cuda-python"));
    }

    #[test]
    fn a_missing_file_is_an_empty_map_but_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SourceMap::load(dir.path()).unwrap().is_empty());
        std::fs::write(SourceMap::path(dir.path()), "this is not toml {{{").unwrap();
        assert!(SourceMap::load(dir.path()).is_err());
    }
}
