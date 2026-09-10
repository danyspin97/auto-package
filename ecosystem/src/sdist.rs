//! Sdist inspection.
//!
//! The two things the PyPI JSON API cannot tell us are both in the sdist:
//! `[build-system] requires` (which tools to install before building) and
//! `[build-system] build-backend` (which tool runs the build). Both come
//! from `pyproject.toml` at the root of the tarball.
//!
//! Old packages without a `pyproject.toml` at all use setuptools implicitly.
//! That is the PEP 517 fallback and the right default.

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

/// The top-level directory name inside a `.tar.gz` sdist.
///
/// A well-formed sdist contains exactly one top-level directory, e.g.
/// `MarkupSafe-3.0.3/`. This is what `%autosetup -n` must match.
pub fn top_level_dir(path: &Path) -> Result<Option<String>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));

    for entry in archive.entries().map_err(Error::Tar)? {
        let entry = entry.map_err(Error::Tar)?;
        let entry_path = entry.path().map_err(Error::Tar)?;

        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }

        if let Some(first) = entry_path.components().next() {
            let name = first.as_os_str().to_string_lossy().into_owned();
            if !name.is_empty() {
                return Ok(Some(name));
            }
        }
    }

    Ok(None)
}

/// List all file paths inside the sdist tarball (relative, sorted).
///
/// Useful for giving the review agent visibility into what the package
/// actually contains — module directories, compiled extensions, etc.
pub fn tarball_files(path: &Path) -> Result<Vec<String>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut files: Vec<String> = archive
        .entries()
        .map_err(Error::Tar)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
        .filter(|p| !p.ends_with('/'))
        .collect();
    files.sort();
    Ok(files)
}

/// Whether the sdist contains C/Cython source files, indicating C extensions.
///
/// Checks for `.c`, `.cpp`, `.cxx`, `.pyx`, `.pxd` files anywhere in the
/// tarball. A package that ships any of these will need a compiler and
/// architecture-specific installation paths.
pub fn has_c_extensions(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let Ok(entries) = archive.entries() else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(entry_path) = entry.path() else {
            continue;
        };
        if entry_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "c" | "cpp" | "cxx" | "pyx" | "pxd"))
        {
            return true;
        }
    }
    false
}

/// Read `[build-system]` from the sdist tarball at `path`.
///
/// Returns the PEP 517 defaults when `pyproject.toml` is absent or when
/// `[build-system]` is missing from it — both cases mean setuptools.
pub fn build_system(path: &Path) -> Result<BuildSystem, Error> {
    let raw = read_pyproject(path)?;
    match raw {
        None => Ok(BuildSystem::setuptools_default()),
        Some(src) => {
            let doc: PyprojectToml = toml::from_str(&src)?;
            Ok(doc
                .build_system
                .map(BuildSystem::from)
                .unwrap_or_else(BuildSystem::setuptools_default))
        }
    }
}

impl BuildSystem {
    fn setuptools_default() -> Self {
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

/// Read `pyproject.toml` from a `.tar.gz` sdist without full extraction.
///
/// The file is always at `<sdist-root>/pyproject.toml` — one directory deep.
/// We reject any entry whose path escapes that layout (absolute paths,
/// `..` components) before opening it.
fn read_pyproject(path: &Path) -> Result<Option<String>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));

    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        let entry_path = entry.path().map_err(Error::Tar)?;

        // Security: reject absolute paths and any `..` component before
        // comparing against the expected layout.
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }

        // pyproject.toml is always exactly one directory deep in a well-formed
        // sdist: `<name>-<version>/pyproject.toml`.
        let components: Vec<_> = entry_path.components().collect();
        if components.len() != 2 {
            continue;
        }
        if components[1].as_os_str() != "pyproject.toml" {
            continue;
        }

        let mut src = String::new();
        entry.read_to_string(&mut src).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        return Ok(Some(src));
    }

    Ok(None)
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
