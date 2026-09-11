//! Sdist inspection.
//!
//! The two things the PyPI JSON API cannot tell us are both in the sdist:
//! `[build-system] requires` (which tools to install before building) and
//! `[build-system] build-backend` (which tool runs the build). Both come
//! from `pyproject.toml` at the root of the tarball.
//!
//! Old packages without a `pyproject.toml` at all use setuptools implicitly.
//! That is the PEP 517 fallback and the right default.
//!
//! The main entry point is [`inspect`], which does a single tarball pass and
//! returns everything callers need. Individual helper functions have been
//! replaced by this to avoid opening and iterating the archive multiple times.

use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use serde::Deserialize;

/// What the sdist says about its own build system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSystem {
    /// The packages that must be installed before the build backend runs.
    /// Maps to `BuildRequires:` in the spec, prefixed with `%{python_module …}`.
    ///
    /// Defaults to `["setuptools"]` when `pyproject.toml` is absent — the PEP
    /// 517 fallback that pip itself applies.
    pub requires: Vec<String>,
    /// The dotted module path of the build backend, e.g. `hatchling.build`.
    /// Informational for now; the generator uses it to pick the right macros.
    pub backend: Option<String>,
}

/// Everything the build pipeline needs from an sdist tarball, gathered in one pass.
#[derive(Debug)]
pub struct SdistInfo {
    pub build_system: BuildSystem,
    /// The single top-level directory inside the tarball, e.g. `MarkupSafe-3.0.3`.
    /// `None` only for malformed archives; well-formed sdists always have one.
    pub top_level_dir: Option<String>,
    pub has_c_extensions: bool,
    pub has_tests: bool,
    /// All file paths in the tarball (non-directories), sorted. Used to give
    /// the review agent visibility into what the package actually contains.
    pub files: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error reading {path}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("tarball entry traversal failed")]
    Tar(#[source] std::io::Error),

    #[error("pyproject.toml is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Extract the sdist tarball into `dest`.
///
/// Every entry in the archive is unpacked under `dest`. Well-formed sdists
/// contain exactly one top-level directory (e.g. `markupsafe-3.0.3/`); after
/// extraction that directory will be at `dest/<top-level>/`.
pub fn extract(tarball: &Path, dest: &Path) -> Result<(), Error> {
    let file = std::fs::File::open(tarball).map_err(|e| Error::Io {
        path: tarball.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    archive.unpack(dest).map_err(Error::Tar)?;
    Ok(())
}

/// Inspect an sdist tarball in a single pass.
///
/// Replaces the individual `build_system`, `top_level_dir`, `has_tests`,
/// `has_c_extensions`, and `tarball_files` helpers — they all iterated the
/// archive separately and caused the tarball to be decompressed 4–5 times.
pub fn inspect(path: &Path) -> Result<SdistInfo, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));

    let mut top_level_dir: Option<String> = None;
    let mut pyproject_src: Option<String> = None;
    let mut has_c = false;
    let mut has_tests = false;
    let mut files: Vec<String> = Vec::new();

    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        // `.into_owned()` so the path doesn't borrow `entry`, allowing the
        // subsequent `read_to_string` call on the same entry.
        let entry_path = entry.path().map_err(Error::Tar)?.into_owned();

        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }

        let path_str = entry_path.to_string_lossy();

        if top_level_dir.is_none() {
            if let Some(first) = entry_path.components().next() {
                let name = first.as_os_str().to_string_lossy().into_owned();
                if !name.is_empty() {
                    top_level_dir = Some(name);
                }
            }
        }

        let components: Vec<_> = entry_path.components().collect();
        if pyproject_src.is_none()
            && components.len() == 2
            && components[1].as_os_str() == "pyproject.toml"
        {
            let mut src = String::new();
            entry.read_to_string(&mut src).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            pyproject_src = Some(src);
            continue;
        }

        if !has_c {
            if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "c" | "cpp" | "cxx" | "pyx" | "pxd") {
                    has_c = true;
                }
            }
        }

        if !has_tests {
            if path_str.contains("/tests/") || path_str.contains("/test/") {
                has_tests = true;
            } else if entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("test_") && n.ends_with(".py"))
            {
                has_tests = true;
            }
        }

        if !path_str.ends_with('/') {
            files.push(path_str.into_owned());
        }
    }

    files.sort();

    let build_system = match pyproject_src {
        None => BuildSystem::setuptools_default(),
        Some(src) => {
            let doc: PyprojectToml = toml::from_str(&src)?;
            doc.build_system
                .map(BuildSystem::from)
                .unwrap_or_else(BuildSystem::setuptools_default)
        }
    };

    Ok(SdistInfo {
        build_system,
        top_level_dir,
        has_c_extensions: has_c,
        has_tests,
        files,
    })
}

impl BuildSystem {
    pub(crate) fn setuptools_default() -> Self {
        Self {
            requires: vec!["setuptools".to_string()],
            backend: Some("setuptools.build_meta".to_string()),
        }
    }
}

impl From<RawBuildSystem> for BuildSystem {
    fn from(raw: RawBuildSystem) -> Self {
        Self {
            requires: raw.requires,
            backend: raw.build_backend,
        }
    }
}

// --- TOML wire types -------------------------------------------------------

#[derive(Deserialize)]
struct PyprojectToml {
    #[serde(rename = "build-system")]
    build_system: Option<RawBuildSystem>,
}

#[derive(Deserialize)]
struct RawBuildSystem {
    #[serde(default)]
    requires: Vec<String>,
    #[serde(rename = "build-backend")]
    build_backend: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_system_from_toml(src: &str) -> BuildSystem {
        let doc: PyprojectToml = toml::from_str(src).unwrap();
        doc.build_system
            .map(BuildSystem::from)
            .unwrap_or_else(BuildSystem::setuptools_default)
    }

    #[test]
    fn hatchling_backend() {
        let src = r#"
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
"#;
        let bs = build_system_from_toml(src);
        assert_eq!(bs.requires, ["hatchling"]);
        assert_eq!(bs.backend.as_deref(), Some("hatchling.build"));
    }

    #[test]
    fn setuptools_with_extras() {
        let src = r#"
[build-system]
requires = ["setuptools>=42", "wheel"]
build-backend = "setuptools.build_meta"
"#;
        let bs = build_system_from_toml(src);
        assert_eq!(bs.requires, ["setuptools>=42", "wheel"]);
    }

    #[test]
    fn no_build_system_section_defaults_to_setuptools() {
        let bs = build_system_from_toml("[project]\nname = \"foo\"\n");
        assert_eq!(bs.requires, ["setuptools"]);
        assert_eq!(bs.backend.as_deref(), Some("setuptools.build_meta"));
    }

    #[test]
    fn no_pyproject_toml_defaults_to_setuptools() {
        // Simulated by calling the default constructor directly — a real
        // tarball without pyproject.toml reaches the same path in build_system().
        let bs = BuildSystem::setuptools_default();
        assert_eq!(bs.requires, ["setuptools"]);
    }

    #[test]
    fn flit_core() {
        let src = r#"
[build-system]
requires = ["flit_core>=3.2.0,<4"]
build-backend = "flit_core.buildapi"
"#;
        let bs = build_system_from_toml(src);
        assert_eq!(bs.requires, ["flit_core>=3.2.0,<4"]);
        assert_eq!(bs.backend.as_deref(), Some("flit_core.buildapi"));
    }
}
