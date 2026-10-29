use super::*;

/// Package name (normalized) → source repository URL.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceMap {
    #[serde(default, rename = "source")]
    pub(super) urls: BTreeMap<String, String>,
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
pub(super) fn key(name: &str) -> String {
    normalize_name(name).replace('-', "_")
}

/// The key earlier versions stored: lowercase, `-`/`.` → `_`, with runs of
/// separators left alone. Entries in an existing `sources.toml` for names such
/// as `foo--bar` live under `foo__bar`, and collapsing runs would orphan them.
pub(super) fn legacy_key(name: &str) -> String {
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
        ap_core::fsx::write_atomic(&path, text)
            .map_err(|e| format!("writing {}: {e}", path.display()))
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
