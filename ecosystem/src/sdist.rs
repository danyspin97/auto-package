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

use std::borrow::Cow;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

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
    /// Name of a test directory sitting directly under the top-level
    /// directory (`tests` or `test`), when there is one — lets the default
    /// `%check` scope `%pytest` to it instead of the whole source tree,
    /// which otherwise also collects anything merely named `test_*.py`
    /// elsewhere (fixture helpers, scripts) as if it were a real test.
    pub top_level_tests_dir: Option<String>,
    /// The name the project declares for itself — `[project] name` in
    /// pyproject.toml, or `[package] name` in Cargo.toml. This is the
    /// authoritative name; a repository directory name is only a guess at it.
    pub project_name: Option<String>,
    /// Where the packaging files live relative to the top-level directory, or
    /// `None` when they are at the top. A repository snapshot of a monorepo
    /// keeps the project in a subdirectory (`cuda_python/`, `python/`), and
    /// the build has to change into it.
    pub project_dir: Option<String>,
    /// The archive carries Python packaging (pyproject.toml/setup.py/setup.cfg).
    pub python_packaging: bool,
    /// The archive carries a Cargo manifest.
    pub cargo_manifest: bool,
    /// A top-level `CMakeLists.txt`, with no Python or Cargo packaging
    /// alongside it — a plain C/C++ library or tool built with CMake.
    pub cmake_manifest: bool,
    /// A top-level `configure.ac`/`configure.in` or `Makefile.am`, with no
    /// Python, Cargo or CMake packaging alongside it — a plain C/C++ project
    /// built with autotools. A release snapshot from a repository tag
    /// (rather than a `make dist` tarball) ships these instead of a
    /// generated `configure` script, so the build needs `autoreconf` first.
    pub autotools_manifest: bool,
    /// Raw `CMakeLists.txt` at the archive root, even when Python or Cargo
    /// packaging also claims the archive (unlike `cmake_manifest`).
    pub cmake_at_root: bool,
    /// Raw autotools manifest at the archive root; see `cmake_at_root`.
    pub autotools_at_root: bool,
    /// The CMake, meson or autotools build of the archive, at its root or in
    /// the one subdirectory that holds a build file, whatever else the
    /// archive carries.
    pub native: Option<NativeBuild>,
    /// Every project found alongside the chosen one, when the archive holds
    /// more than one. Empty for an ordinary single-project sdist.
    ///
    /// A monorepo usually wants all of them packaged, as subpackages of one
    /// source package, rather than one build per subdirectory.
    pub subprojects: Vec<Subproject>,
    /// All file paths in the tarball (non-directories), sorted. Used to give
    /// the review agent visibility into what the package actually contains.
    pub files: Vec<String>,
    /// Command names the chosen `pyproject.toml` declares in `[project.scripts]`
    /// / `[project.gui-scripts]` (or `[tool.poetry.scripts]`), plain file names
    /// only. pip generates one `/usr/bin/<name>` per flavor for each.
    pub console_scripts: Vec<String>,
}

/// One project inside a multi-project archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subproject {
    /// Directory relative to the archive's top-level directory.
    pub dir: String,
    /// The name it declares for itself, when it declares one.
    pub name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error reading {path}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("tarball entry traversal failed: {0}")]
    Tar(#[source] std::io::Error),

    #[error("archive exceeds extraction limit: {0}")]
    LimitExceeded(String),

    #[error("refusing link {link} -> {target}: it would reach outside the archive")]
    LinkEscapes {
        link: std::path::PathBuf,
        target: std::path::PathBuf,
    },

    #[error("pyproject.toml is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),

    /// The disk cannot take the extraction (see [`crate::space`]).
    #[error("{0}")]
    NoSpace(#[from] crate::space::NotEnoughSpace),
}

impl Error {
    /// Whether the failure is a full disk, from our own check or from the OS.
    pub fn is_disk_full(&self) -> bool {
        match self {
            Error::NoSpace(_) => true,
            Error::Io { source, .. } | Error::Tar(source) => ap_core::fsx::is_disk_full(source),
            _ => false,
        }
    }
}

mod archive;
mod elf;
mod extract;
mod inspect;
mod manifest;
mod native;
mod requirements;
mod safety;
mod wheel;

#[cfg(test)]
mod source_repo_tests;
#[cfg(test)]
mod tests;

use archive::*;
use elf::*;
pub use extract::*;
pub use inspect::*;
pub use manifest::*;
pub use native::{NativeBuild, NativeKind};
pub use requirements::*;
pub use safety::*;
pub use wheel::*;
