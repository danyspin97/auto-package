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

/// How an archive is wrapped on disk.
///
/// PyPI's historical default is `.tar.gz`, but every other shape in this list
/// still turns up: `.zip` is an older sdist form the index still accepts,
/// `.tar.bz2` lives on in pre-2015 uploads, and `.tar.zst` is what the Rust
/// `ap_service` emits when it hands tarballs back for inspection. Previously
/// the whole module hard-coded gzip and only wheels took the unzip path, so
/// any of these other formats crashed inspection before the pipeline could
/// even report what it had received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    TarGz,
    TarBz2,
    TarZstd,
    /// A `.zip` sdist. Container-wise indistinguishable from `.whl`; kept
    /// separate for diagnostics.
    Zip,
    /// A Python wheel — zip container, no top-level directory wrapper.
    Wheel,
}

fn classify(path: &Path) -> Option<ArchiveKind> {
    let s = path.to_string_lossy().to_lowercase();
    if s.ends_with(".tar.gz") || s.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if s.ends_with(".tar.bz2") || s.ends_with(".tbz2") {
        Some(ArchiveKind::TarBz2)
    } else if s.ends_with(".tar.zst") || s.ends_with(".tzst") {
        Some(ArchiveKind::TarZstd)
    } else if s.ends_with(".whl") {
        Some(ArchiveKind::Wheel)
    } else if s.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

/// Open a compressed-tar archive as a boxed streaming reader.
///
/// Returned `Read` is a reader over the uncompressed tar bytes, ready for
/// `tar::Archive::new`. The kind is passed in rather than re-sniffed so one
/// callsite can decide what the archive is and both `extract` and
/// `inspect_preferring` reuse the same answer.
fn open_tar(path: &Path, kind: ArchiveKind) -> Result<tar::Archive<Box<dyn Read>>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader: Box<dyn Read> = match kind {
        ArchiveKind::TarGz => Box::new(GzDecoder::new(file)),
        ArchiveKind::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        ArchiveKind::TarZstd => {
            let dec = zstd::Decoder::new(file).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Box::new(dec)
        }
        ArchiveKind::Zip | ArchiveKind::Wheel => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("zip-shaped archive is not a tar; call the zip path"),
            });
        }
    };
    Ok(tar::Archive::new(reader))
}

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
    /// Every project found alongside the chosen one, when the archive holds
    /// more than one. Empty for an ordinary single-project sdist.
    ///
    /// A monorepo usually wants all of them packaged, as subpackages of one
    /// source package, rather than one build per subdirectory.
    pub subprojects: Vec<Subproject>,
    /// All file paths in the tarball (non-directories), sorted. Used to give
    /// the review agent visibility into what the package actually contains.
    pub files: Vec<String>,
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

    #[error("pyproject.toml is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Extraction caps: a hostile or corrupt archive must not exhaust the disk
/// or the inode table. Generous enough for any real sdist or CUDA wheel.
const MAX_ENTRIES: usize = 200_000;
const MAX_TOTAL_BYTES: u64 = 4 << 30;
const MAX_FILE_BYTES: u64 = 2 << 30;
/// Cap on a manifest read during inspection.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// Running totals checked against the extraction caps.
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
}

impl Budget {
    fn charge(&mut self, size: u64) -> Result<(), Error> {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(size);
        if self.entries > MAX_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "more than {MAX_ENTRIES} entries"
            )));
        }
        if size > MAX_FILE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "entry of {size} bytes (limit {MAX_FILE_BYTES})"
            )));
        }
        if self.bytes > MAX_TOTAL_BYTES {
            return Err(Error::LimitExceeded(format!(
                "more than {MAX_TOTAL_BYTES} uncompressed bytes"
            )));
        }
        Ok(())
    }

    /// Copy one entry's content to `out`, bounded by what the budget still
    /// allows rather than by the (attacker-controlled) declared size.
    ///
    /// `declared` is what [`Budget::charge`] already took for this entry. A
    /// header that understates the real size would otherwise let one entry
    /// expand to the per-file cap and sidestep the total cap; here the bytes
    /// actually written are what count.
    fn copy_entry(
        &mut self,
        declared: u64,
        input: &mut impl Read,
        out: &mut impl std::io::Write,
    ) -> Result<(), Error> {
        let before = self.bytes.saturating_sub(declared);
        let allowed = MAX_FILE_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(before));
        // Read one byte past the allowance so overshoot is detectable.
        let written =
            std::io::copy(&mut input.take(allowed.saturating_add(1)), out).map_err(|e| {
                Error::Io {
                    path: std::path::PathBuf::new(),
                    source: e,
                }
            })?;
        if written > allowed {
            return Err(Error::LimitExceeded(format!(
                "entry expands past its declared {declared} bytes beyond the extraction limit"
            )));
        }
        self.bytes = before.saturating_add(written.max(declared));
        Ok(())
    }
}

/// Read up to [`MAX_MANIFEST_BYTES`] of `entry` as lossy UTF-8. `None` when
/// the read fails — an unreadable nested manifest must not fail inspection.
fn read_manifest(entry: &mut impl Read) -> Option<String> {
    let mut buf = Vec::new();
    entry.take(MAX_MANIFEST_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// What a wheel installs, read from its file list — enough to write a correct
/// `%check` without the agent rediscovering it from a failed build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WheelLayout {
    /// A name `import` can resolve at the top level: a `<pkg>/__init__.py`
    /// directory or a `<mod>.py` module. `None` for namespace-only or
    /// binary-only wheels (NVIDIA's `nvidia/` with just `lib/*.so`).
    pub importable: Option<String>,
    /// Shortest path (relative to site-packages) of a shared library in the
    /// wheel, for a file-existence check when there is nothing to import.
    pub first_shared_lib: Option<String>,
    /// Directories (relative to site-packages, no trailing slash) containing
    /// a bundled C/C++ header (`.h`/`.hpp`) outside `.dist-info`/`.data` —
    /// a prebuilt binary wheel shipping its own public API headers
    /// alongside the library (NVIDIA's CUDA wheels all do this: `nccl.h`,
    /// `cublas_v2.h`, …), not stray third-party content. rpmlint's
    /// `devel-file-in-non-devel-package` fires on every one of these in a
    /// package that — being a prebuilt-wheel repackage, convention 27 —
    /// never gets a `-devel` split, since nothing in this build environment
    /// ever compiles against them. Sorted and deduplicated.
    pub header_dirs: Vec<String>,
}

/// Inspect a wheel's file list. `None` when it cannot be read as a zip.
pub fn wheel_layout(wheel: &Path) -> Option<WheelLayout> {
    let file = std::fs::File::open(wheel).ok()?;
    let zip_archive = zip::ZipArchive::new(file).ok()?;
    let names: Vec<String> = zip_archive.file_names().map(str::to_string).collect();
    let top_of = |n: &str| n.split('/').next().unwrap_or("").to_string();
    let skip =
        |top: &str| top.ends_with(".dist-info") || top.ends_with(".data") || top == "__pycache__";
    let mut importable = None;
    for n in &names {
        let top = top_of(n);
        if skip(&top) {
            continue;
        }
        let is_pkg = n == &format!("{top}/__init__.py");
        let is_mod = n.ends_with(".py") && !n.contains('/');
        if is_pkg {
            importable = Some(top);
            break;
        }
        if is_mod && importable.is_none() {
            importable = Some(n.trim_end_matches(".py").to_string());
        }
    }
    let first_shared_lib = names
        .iter()
        .filter(|n| {
            let file = n.rsplit('/').next().unwrap_or("");
            !skip(&top_of(n)) && (file.ends_with(".so") || file.contains(".so."))
        })
        .min_by_key(|n| n.len())
        .cloned();
    let header_dirs: Vec<String> = {
        let mut dirs: Vec<String> = names
            .iter()
            .filter(|n| {
                let file = n.rsplit('/').next().unwrap_or("");
                !skip(&top_of(n)) && (file.ends_with(".h") || file.ends_with(".hpp"))
            })
            .filter_map(|n| n.rsplit_once('/').map(|(dir, _)| dir.to_string()))
            .collect();
        dirs.sort_unstable();
        dirs.dedup();
        // Collapse to the shallowest covering set: a real header tree (NCCL's
        // `include/nccl_device/gin/gdaki/...`) nests many directories under
        // one top-level `include/`, and rpmlint's `addFilter` is an
        // unanchored substring match — a filter for the parent already
        // matches every path under it, so emitting one per nested
        // subdirectory too would just be redundant noise. Sorting first
        // guarantees a parent directory always sorts immediately before its
        // descendants (it's a string prefix of each one), so checking only
        // the most recently kept entry is enough to drop every descendant.
        let mut kept: Vec<String> = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let is_nested_under_kept = kept.last().is_some_and(|parent: &String| {
                dir.starts_with(parent.as_str()) && dir.as_bytes().get(parent.len()) == Some(&b'/')
            });
            if !is_nested_under_kept {
                kept.push(dir);
            }
        }
        kept
    };
    Some(WheelLayout {
        importable,
        first_shared_lib,
        header_dirs,
    })
}

/// The `Root-Is-Purelib` value from a wheel's `*.dist-info/WHEEL` file.
///
/// This — not the platform tag — decides where `pip install` puts a wheel's
/// files: `true` means `purelib` (`%{python_sitelib}`), `false` means
/// `platlib` (`%{python_sitearch}`). NVIDIA's CUDA wheels carry a
/// `py3-none-manylinux_*` tag (they ship `.so` files) yet say `true`, so their
/// files land in sitelib. `None` when the file cannot be read.
pub fn wheel_root_is_purelib(wheel: &Path) -> Option<bool> {
    let file = std::fs::File::open(wheel).ok()?;
    let mut zip_archive = zip::ZipArchive::new(file).ok()?;
    let name = (0..zip_archive.len()).find_map(|i| {
        let n = zip_archive.name_for_index(i)?;
        (n.ends_with(".dist-info/WHEEL") && n.matches('/').count() == 1).then(|| n.to_string())
    })?;
    let entry = zip_archive.by_name(&name).ok()?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(entry, 64 * 1024), &mut text).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("Root-Is-Purelib")
            .then(|| v.trim().eq_ignore_ascii_case("true"))
    })
}

/// Extract the sdist tarball into `dest`.
///
/// Every entry in the archive is unpacked under `dest`. Well-formed sdists
/// contain exactly one top-level directory (e.g. `markupsafe-3.0.3/`); after
/// extraction that directory will be at `dest/<top-level>/`.
///
/// Dispatches on the file extension — `.tar.gz`/`.tgz`, `.tar.bz2`/`.tbz2`,
/// `.tar.zst`/`.tzst`, `.zip`, and `.whl` are all accepted. A `.whl` (and a
/// bare `.zip`) is a zip archive with no wrapping top-level directory — it
/// unpacks flat into `dest` itself. The gzip, bzip2 and zstd paths all share
/// the tar iterator; only the outer decoder differs.
pub fn extract(tarball: &Path, dest: &Path) -> Result<(), Error> {
    let kind = classify(tarball).ok_or_else(|| Error::Io {
        path: tarball.to_path_buf(),
        source: std::io::Error::other(format!(
            "unrecognised archive extension on {}: expected one of \
             .tar.gz/.tgz, .tar.bz2/.tbz2, .tar.zst/.tzst, .zip, .whl",
            tarball.display(),
        )),
    })?;
    match kind {
        ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarZstd => {
            let mut archive = open_tar(tarball, kind)?;
            std::fs::create_dir_all(dest).map_err(|e| Error::Io {
                path: dest.to_path_buf(),
                source: e,
            })?;
            let mut budget = Budget::default();
            // `unpack_in` keeps tar-rs's traversal and symlink protections.
            for entry in archive.entries().map_err(Error::Tar)? {
                let mut entry = entry.map_err(Error::Tar)?;
                budget.charge(entry.size())?;
                if let Some(reason) = escaping_link(&entry) {
                    tracing::warn!("skipping tar entry: {reason}");
                    continue;
                }
                entry.unpack_in(dest).map_err(Error::Tar)?;
            }
            Ok(())
        }
        ArchiveKind::Zip | ArchiveKind::Wheel => extract_zip(tarball, dest),
    }
}

/// Why a symlink or hard-link entry must not be materialised, or `None` when
/// it is not a link or stays inside the extraction root.
///
/// tar-rs creates the link with whatever target the archive names, so an
/// absolute or `../` target would leave a link in the build tree that points
/// outside it, and later steps that follow it would read or write there.
/// The check is lexical: it cannot follow links that do not exist yet.
fn escaping_link<R: Read>(entry: &tar::Entry<'_, R>) -> Option<String> {
    use std::path::Component;
    let kind = entry.header().entry_type();
    if !kind.is_symlink() && !kind.is_hard_link() {
        return None;
    }
    let name = entry.path().ok()?.into_owned();
    let target = match entry.link_name() {
        Ok(Some(t)) => t.into_owned(),
        _ => return Some(format!("link {} has no readable target", name.display())),
    };
    let refuse = || {
        Some(format!(
            "link {} -> {} points outside the extraction root",
            name.display(),
            target.display()
        ))
    };
    if target.is_absolute() {
        return refuse();
    }
    // A symlink target is relative to the link's directory; a hard-link
    // target is relative to the archive root.
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    if kind.is_symlink() {
        let parent = name.parent().unwrap_or(Path::new(""));
        for c in parent.components() {
            match c {
                Component::Normal(p) => parts.push(p),
                Component::ParentDir => {
                    if parts.pop().is_none() {
                        return refuse();
                    }
                }
                _ => {}
            }
        }
    }
    for c in target.components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return refuse();
                }
            }
            Component::CurDir => {}
            _ => return refuse(),
        }
    }
    None
}

/// Extract a zip-shaped archive (`.whl` or `.zip`) using the `zip` crate.
///
/// Pure-Rust, no reliance on the system `unzip` binary — the previous
/// implementation shelled out, which quietly failed on build hosts without
/// InfoZip installed and left no artifact behind to diagnose from.
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), Error> {
    let file = std::fs::File::open(archive).map_err(|e| Error::Io {
        path: archive.to_path_buf(),
        source: e,
    })?;
    let mut zip_archive = zip::ZipArchive::new(file).map_err(|e| Error::Io {
        path: archive.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    std::fs::create_dir_all(dest).map_err(|e| Error::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;
    let mut budget = Budget::default();
    for i in 0..zip_archive.len() {
        let mut entry = zip_archive.by_index(i).map_err(|e| Error::Io {
            path: archive.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        })?;
        budget.charge(entry.size())?;
        // `enclosed_name` filters out absolute paths and `..` traversal — same
        // guard the tar reader applies — so a hostile wheel can't escape
        // `dest`.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| Error::Io {
                path: out_path.clone(),
                source: e,
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        // Never materialise a symlink entry: its "content" is the link
        // target, and writing it as a file (or linking out of `dest`) is
        // useless at best and a traversal vector at worst.
        if entry.is_symlink() {
            continue;
        }
        let mode = entry.unix_mode();
        let mut out_file = std::fs::File::create(&out_path).map_err(|e| Error::Io {
            path: out_path.clone(),
            source: e,
        })?;
        let declared = entry.size();
        budget
            .copy_entry(declared, &mut entry, &mut out_file)
            .map_err(|e| match e {
                Error::Io { source, .. } => Error::Io {
                    path: out_path.clone(),
                    source,
                },
                other => other,
            })?;
        drop(out_file);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            // Keep rwx bits only (drop setuid/setgid/sticky), and always
            // owner-readable/writable so later steps can handle the file.
            let perms = std::fs::Permissions::from_mode((mode & 0o777) | 0o600);
            std::fs::set_permissions(&out_path, perms).map_err(|e| Error::Io {
                path: out_path.clone(),
                source: e,
            })?;
        }
    }
    Ok(())
}

/// Inspect an sdist tarball in a single pass.
///
/// Replaces the individual `build_system`, `top_level_dir`, `has_tests`,
/// `has_c_extensions`, and `tarball_files` helpers — they all iterated the
/// archive separately and caused the tarball to be decompressed 4–5 times.
pub fn inspect(path: &Path) -> Result<SdistInfo, Error> {
    inspect_preferring(path, None)
}

/// Inspect an archive, preferring the subproject named `want`.
///
/// A monorepo snapshot holds several projects side by side — cuda-python ships
/// `cuda_bindings/`, `cuda_core/` and `cuda_python/` — and which one is "the"
/// project is only answerable by knowing which one was asked for. Depth still
/// wins first: a manifest at the archive root is the project, whatever it is
/// called.
pub fn inspect_preferring(path: &Path, want: Option<&str>) -> Result<SdistInfo, Error> {
    // Compare directory names the way pip does, so `cuda-python` finds
    // `cuda_python/`.
    let normalized = want.map(|w| w.to_lowercase().replace(['-', '.'], "_"));
    // Pick the right decoder up front: a `.tar.zst` fed through GzDecoder
    // produces nothing and inspection reports no manifests, so dispatch on
    // the extension before touching the stream.
    let kind = classify(path).ok_or_else(|| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(format!(
            "unrecognised archive extension on {}: expected one of \
             .tar.gz/.tgz, .tar.bz2/.tbz2, .tar.zst/.tzst, .zip, .whl",
            path.display(),
        )),
    })?;
    let entries = match kind {
        ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarZstd => {
            collect_tar_entries(path, kind)?
        }
        ArchiveKind::Zip | ArchiveKind::Wheel => collect_zip_entries(path)?,
    };
    inspect_from_entries(entries, normalized.as_deref())
}

/// One archive entry, flattened into the shape the inspection body needs.
///
/// `contents` is pre-read for the manifest files that the body actually
/// opens (pyproject.toml, Cargo.toml) and `None` for everything else — the
/// body never pulls bytes out of entries it doesn't need, so skipping the
/// read for ordinary source files keeps the memory profile the same as the
/// previous streaming implementation.
struct InspectEntry {
    path_str: String,
    is_dir: bool,
    contents: Option<String>,
}

fn collect_tar_entries(path: &Path, kind: ArchiveKind) -> Result<Vec<InspectEntry>, Error> {
    let mut archive = open_tar(path, kind)?;
    let mut out = Vec::new();
    // Same caps as extraction: inspect must not be the cheaper way to make a
    // hostile archive burn memory (the entry list) or time.
    let mut budget = Budget::default();
    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        budget.charge(entry.size())?;
        let entry_path = entry.path().map_err(Error::Tar)?.into_owned();
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }
        let path_str = entry_path.to_string_lossy().into_owned();
        let is_dir = path_str.ends_with('/');
        let file_name = entry_path.file_name().and_then(|n| n.to_str());
        let contents = if matches!(file_name, Some("pyproject.toml" | "Cargo.toml")) {
            read_manifest(&mut entry)
        } else {
            None
        };
        out.push(InspectEntry {
            path_str,
            is_dir,
            contents,
        });
    }
    Ok(out)
}

/// Walk a zip-shaped archive (`.zip` or `.whl`) producing the same
/// `InspectEntry` list that `collect_tar_entries` builds for tar-shaped
/// archives. A `.whl` has no wrapping top-level directory — the depth
/// bookkeeping in `inspect_from_entries` handles that automatically.
fn collect_zip_entries(path: &Path) -> Result<Vec<InspectEntry>, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut zip_archive = zip::ZipArchive::new(file).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    let mut out = Vec::new();
    let mut budget = Budget::default();
    for i in 0..zip_archive.len() {
        let mut entry = zip_archive.by_index(i).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        })?;
        budget.charge(entry.size())?;
        let name = entry.name().to_string();
        // Mirror the tar guard: skip absolute paths and `..` traversal.
        if std::path::Path::new(&name).is_absolute()
            || std::path::Path::new(&name)
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }
        let is_dir = entry.is_dir();
        let file_name = std::path::Path::new(&name)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        let contents =
            if !is_dir && matches!(file_name.as_deref(), Some("pyproject.toml" | "Cargo.toml")) {
                read_manifest(&mut entry)
            } else {
                None
            };
        out.push(InspectEntry {
            path_str: name,
            is_dir,
            contents,
        });
    }
    Ok(out)
}

fn inspect_from_entries(
    entries: Vec<InspectEntry>,
    normalized: Option<&str>,
) -> Result<SdistInfo, Error> {
    let wanted = |components: &[std::path::Component<'_>]| -> bool {
        let (Some(want), true) = (normalized, components.len() >= 3) else {
            return false;
        };
        components[components.len() - 2]
            .as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .replace(['-', '.'], "_")
            == want
    };

    let mut top_level_dir: Option<String> = None;
    // Shallowest manifest of each kind, with its depth below the top level.
    // Shallowest wins: a vendored dependency's Cargo.toml is not the project's.
    // Every Python manifest, with its depth below the top level and the
    // directory it sits in. The winner is picked afterwards so the losers are
    // still available: in a monorepo they are the sibling subprojects.
    let mut py_manifests: Vec<(usize, String, String)> = Vec::new();
    let mut cargo: Option<((usize, u8), String)> = None;
    let mut setup_py_depth: Option<usize> = None;
    let mut cmake_manifest = false;
    let mut autotools_manifest = false;
    let mut has_c = false;
    let mut has_tests = false;
    let mut top_level_tests_dir: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    for entry in entries {
        // `./thing-1.0/x` is the same entry as `thing-1.0/x`; left alone its
        // first component is `.` and the top-level directory comes out as ".".
        let path_str = &entry.path_str.trim_start_matches("./").to_string();
        if path_str.is_empty() {
            continue;
        }
        let entry_path = std::path::PathBuf::from(path_str);

        if !entry.is_dir {
            files.push(path_str.clone());
        }

        let components: Vec<_> = entry_path.components().collect();

        // The top-level directory is the first component of any entry that
        // actually sits *inside* a directory. Taking the first entry's first
        // component instead picks up `pax_global_header` — the pseudo-entry
        // git-archive (and therefore every GitHub source tarball) writes
        // first — and every such spec ends up with
        // `%autosetup -n pax_global_header`.
        if top_level_dir.is_none() && components.len() >= 2 {
            let name = components[0].as_os_str().to_string_lossy().into_owned();
            if !name.is_empty() {
                top_level_dir = Some(name);
            }
        }
        // Depth below the top-level directory: 0 means the archive root.
        let depth = components.len().saturating_sub(2);
        let file_name = components.last().map(|c| c.as_os_str());

        let rank = (depth, if wanted(&components) { 0 } else { 1 });

        if file_name == Some("pyproject.toml".as_ref())
            && let Some(src) = &entry.contents
        {
            let dir = components[1..components.len() - 1]
                .iter()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            py_manifests.push((depth, dir, src.clone()));
            continue;
        }

        if file_name == Some("Cargo.toml".as_ref())
            && cargo.as_ref().is_none_or(|(r, _)| rank < *r)
            && let Some(src) = &entry.contents
        {
            cargo = Some((rank, src.clone()));
            continue;
        }

        if matches!(file_name, Some(n) if n == "setup.py" || n == "setup.cfg")
            && setup_py_depth.is_none_or(|d| depth < d)
        {
            setup_py_depth = Some(depth);
        }

        if depth == 0 && file_name == Some("CMakeLists.txt".as_ref()) {
            cmake_manifest = true;
        }
        if depth == 0
            && matches!(file_name, Some(n) if n == "configure.ac" || n == "configure.in" || n == "Makefile.am")
        {
            autotools_manifest = true;
        }

        if !has_c
            && let Some(ext) = entry_path.extension().and_then(|e| e.to_str())
            && matches!(ext, "c" | "cpp" | "cxx" | "pyx" | "pxd")
        {
            has_c = true;
        }

        if !has_tests
            && (path_str.contains("/tests/")
                || path_str.contains("/test/")
                || entry_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("test_") && n.ends_with(".py")))
        {
            has_tests = true;
        }

        if top_level_tests_dir.is_none()
            && components.len() >= 3
            && let Some(name) = components[1].as_os_str().to_str()
            && matches!(name, "tests" | "test")
        {
            top_level_tests_dir = Some(name.to_string());
        }
    }

    files.sort();

    // A root CMake/autotools project that vendors or binds other languages in
    // subdirectories (`bindings/python/setup.py`, `third_party/x/Cargo.toml`)
    // is still a CMake/autotools project: only a root-level manifest of these
    // kinds may compete with it.
    if cmake_manifest || autotools_manifest {
        if setup_py_depth.is_some_and(|d| d > 0) {
            setup_py_depth = None;
        }
        if cargo.as_ref().is_some_and(|((d, _), _)| *d > 0) {
            cargo = None;
        }
    }

    // Shallowest wins; among equals, the one that was asked for.
    let declared_name = |src: &str| {
        toml::from_str::<PyprojectToml>(src)
            .ok()
            .and_then(PyprojectToml::declared_name)
    };
    let rank_of = |depth: usize, dir: &str| {
        let norm = dir.to_lowercase().replace(['-', '.'], "_");
        (depth, normalized != Some(norm.as_str()))
    };
    py_manifests.sort_by(|a, b| {
        rank_of(a.0, &a.1)
            .cmp(&rank_of(b.0, &b.1))
            .then_with(|| a.1.cmp(&b.1))
    });
    let pyproject = py_manifests.first().cloned();

    let mut project_name = None;
    let build_system = match &pyproject {
        None => BuildSystem::setuptools_default(),
        Some((_, _, src)) => {
            // One broken pyproject.toml must not abort the whole inspect: a
            // setup.py alongside it still describes a buildable project.
            match toml::from_str::<PyprojectToml>(src) {
                Ok(mut doc) => {
                    let build_system = doc
                        .build_system
                        .take()
                        .map(BuildSystem::from)
                        .unwrap_or_else(BuildSystem::setuptools_default);
                    project_name = doc.declared_name();
                    build_system
                }
                Err(e) => {
                    tracing::warn!("ignoring pyproject.toml metadata: invalid TOML: {e}");
                    BuildSystem::setuptools_default()
                }
            }
        }
    };

    // Siblings of the chosen project: same depth, and only when that depth is
    // below the root — a single project at the top is not a monorepo.
    let primary_depth = pyproject.as_ref().map(|(d, _, _)| *d).unwrap_or(0);
    let mut subprojects: Vec<Subproject> = if primary_depth == 0 {
        Vec::new()
    } else {
        py_manifests
            .iter()
            .filter(|(d, _, _)| *d == primary_depth)
            .map(|(_, dir, src)| Subproject {
                dir: dir.clone(),
                name: declared_name(src),
            })
            .collect()
    };
    subprojects.sort_by(|a, b| a.dir.cmp(&b.dir));
    if subprojects.len() < 2 {
        subprojects.clear();
    }

    // Only fall back to the crate name when there is no Python name: a maturin
    // project has both manifests and the Python one is the distribution.
    if project_name.is_none()
        && let Some((_, src)) = &cargo
        && let Ok(doc) = toml::from_str::<CargoToml>(src)
    {
        project_name = doc.package.and_then(|p| p.name);
    }

    let python_depth = match (pyproject.as_ref().map(|(d, _, _)| *d), setup_py_depth) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let project_depth = python_depth.or(cargo.as_ref().map(|((d, _), _)| *d));

    // Name the subdirectory the packaging lives in, by finding a file at that
    // depth and taking its leading path components below the top level.
    let project_dir = match project_depth {
        None | Some(0) => None,
        Some(depth) => {
            let mut dirs: Vec<String> = files
                .iter()
                .filter_map(|f| {
                    let rest = f.split_once('/').map(|(_, r)| r)?;
                    let parts: Vec<&str> = rest.split('/').collect();
                    (parts.len() == depth + 1
                        && matches!(
                            *parts.last()?,
                            "pyproject.toml" | "setup.py" | "setup.cfg" | "Cargo.toml"
                        ))
                    .then(|| parts[..depth].join("/"))
                })
                .collect();
            // Same tie-break as the manifest choice above.
            dirs.sort_by_key(|d| {
                let norm = d.to_lowercase().replace(['-', '.'], "_");
                (normalized != Some(norm.as_str()), d.clone())
            });
            dirs.into_iter().next()
        }
    };

    Ok(SdistInfo {
        build_system,
        top_level_dir,
        has_c_extensions: has_c,
        has_tests,
        top_level_tests_dir,
        project_name,
        project_dir,
        python_packaging: python_depth.is_some(),
        cargo_manifest: cargo.is_some(),
        // A plain C/C++ project only when nothing else already claims the
        // archive — a maturin/PyO3 crate has both Cargo.toml and
        // pyproject.toml, and a CMake-based Python extension (scikit-build)
        // has both CMakeLists.txt and pyproject.toml. Python and Cargo win.
        cmake_at_root: cmake_manifest,
        autotools_at_root: autotools_manifest,
        cmake_manifest: cmake_manifest && python_depth.is_none() && cargo.is_none(),
        autotools_manifest: autotools_manifest && python_depth.is_none() && cargo.is_none(),
        subprojects,
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

    pub fn is_maturin(&self) -> bool {
        self.backend.as_deref() == Some("maturin")
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
    project: Option<RawProject>,
    // Deliberately loose: one malformed `[tool.*]` field must not fail the
    // whole parse. Only `tool.poetry.name` is read, and only if a string.
    tool: Option<toml::Value>,
}

#[derive(Deserialize)]
struct RawProject {
    name: Option<String>,
}

impl PyprojectToml {
    /// `[project].name`, else the legacy `[tool.poetry].name`.
    fn declared_name(self) -> Option<String> {
        self.project.and_then(|p| p.name).or_else(|| {
            self.tool?
                .get("poetry")?
                .get("name")?
                .as_str()
                .map(str::to_string)
        })
    }
}

#[derive(Deserialize)]
struct CargoToml {
    package: Option<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: Option<String>,
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

    /// Every entry lands in `files`, including the manifests — callers decide
    /// what a tarball is by looking for `pyproject.toml` and `Cargo.toml`
    /// there, and reading one must not remove it from the listing.
    #[test]
    fn the_manifests_appear_in_the_file_listing() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "thing-1.0/Cargo.toml",
                    b"[package]\nname = \"thing\"\n".as_slice(),
                ),
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing-py\"\n".as_slice(),
                ),
                ("thing-1.0/src/main.rs", b"fn main() {}".as_slice()),
            ],
        );

        let info = inspect(&tarball).unwrap();
        assert!(
            info.files.iter().any(|f| f == "thing-1.0/Cargo.toml"),
            "{:?}",
            info.files
        );
        assert!(
            info.files.iter().any(|f| f == "thing-1.0/pyproject.toml"),
            "{:?}",
            info.files
        );
        // Both manifests present: the Python name wins (maturin projects).
        assert_eq!(info.project_name.as_deref(), Some("thing-py"));
    }

    #[test]
    fn a_poetry_only_pyproject_still_yields_its_declared_name() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "llama.cpp-b1/pyproject.toml",
                    b"[tool.poetry]\nname = \"llama-cpp-scripts\"\n".as_slice(),
                ),
                (
                    "llama.cpp-b1/CMakeLists.txt",
                    b"project(llama)\n".as_slice(),
                ),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert!(info.python_packaging);
        assert!(info.cmake_at_root);
        assert!(!info.cmake_manifest);
        assert_eq!(info.project_name.as_deref(), Some("llama-cpp-scripts"));
    }

    #[test]
    fn a_top_level_cmakelists_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "libthing-1.0/CMakeLists.txt",
                    b"project(thing)\n".as_slice(),
                ),
                ("libthing-1.0/src/thing.c", b"int main(){}".as_slice()),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert!(info.cmake_manifest);
        assert!(!info.autotools_manifest);
    }

    #[test]
    fn top_level_tests_dir_is_found_but_a_nested_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n".as_slice(),
                ),
                ("thing-1.0/tests/test_thing.py", b"".as_slice()),
                // A nested test-looking file elsewhere must not win over the
                // real top-level tests/ dir, nor substitute for it when the
                // real one is absent.
                ("thing-1.0/thing/scripts/test_fixture.py", b"".as_slice()),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert_eq!(info.top_level_tests_dir.as_deref(), Some("tests"));

        let tarball2 = dir.path().join("src2.tar.gz");
        write_tar(
            &tarball2,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n".as_slice(),
                ),
                ("thing-1.0/thing/scripts/test_fixture.py", b"".as_slice()),
            ],
        );
        let info2 = inspect(&tarball2).unwrap();
        assert_eq!(info2.top_level_tests_dir, None);
    }

    #[test]
    fn a_nested_cmakelists_is_not_the_project() {
        // A vendored dependency's CMakeLists.txt two levels down must not
        // make an otherwise plain sdist look like a CMake project.
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n".as_slice(),
                ),
                (
                    "thing-1.0/vendor/lib/CMakeLists.txt",
                    b"project(vendored)\n".as_slice(),
                ),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert!(!info.cmake_manifest);
    }

    #[test]
    fn a_top_level_configure_ac_is_detected_as_autotools() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "libthing-1.0/configure.ac",
                    b"AC_INIT([thing], [1.0])\n".as_slice(),
                ),
                (
                    "libthing-1.0/Makefile.am",
                    b"bin_PROGRAMS = thing\n".as_slice(),
                ),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert!(info.autotools_manifest);
        assert!(!info.cmake_manifest);
    }

    #[test]
    fn cmake_alongside_python_packaging_is_not_a_plain_c_project() {
        // scikit-build-core Python extensions carry both — Python wins, per
        // convention 3, not the new plain-C/C++ path.
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n".as_slice(),
                ),
                ("thing-1.0/CMakeLists.txt", b"project(thing)\n".as_slice()),
            ],
        );
        let info = inspect(&tarball).unwrap();
        assert!(!info.cmake_manifest);
        assert!(info.python_packaging);
    }

    #[test]
    fn a_crate_reports_its_cargo_name() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[(
                "ripgrep-14.1.1/Cargo.toml",
                b"[package]\nname = \"ripgrep\"\n".as_slice(),
            )],
        );
        assert_eq!(
            inspect(&tarball).unwrap().project_name.as_deref(),
            Some("ripgrep")
        );
    }

    fn write_tar(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, name, *body).unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap();
    }

    /// A monorepo snapshot holds several projects side by side; which one is
    /// "the" project is only answerable by knowing which was asked for.
    #[test]
    fn a_monorepo_yields_the_subproject_that_was_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "cuda-python-13.0.2/cuda_bindings/pyproject.toml",
                    b"[project]\nname = \"cuda-bindings\"\n".as_slice(),
                ),
                (
                    "cuda-python-13.0.2/cuda_core/pyproject.toml",
                    b"[project]\nname = \"cuda-core\"\n".as_slice(),
                ),
                (
                    "cuda-python-13.0.2/cuda_python/pyproject.toml",
                    b"[project]\nname = \"cuda-python\"\n".as_slice(),
                ),
            ],
        );

        let info = inspect_preferring(&tarball, Some("cuda-python")).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("cuda-python"));
        assert_eq!(info.project_dir.as_deref(), Some("cuda_python"));
        assert!(info.python_packaging);

        // The siblings are not dropped — a monorepo usually wants all of them.
        assert_eq!(
            info.subprojects,
            vec![
                Subproject {
                    dir: "cuda_bindings".into(),
                    name: Some("cuda-bindings".into())
                },
                Subproject {
                    dir: "cuda_core".into(),
                    name: Some("cuda-core".into())
                },
                Subproject {
                    dir: "cuda_python".into(),
                    name: Some("cuda-python".into())
                },
            ]
        );

        // Asking for a sibling gets that sibling.
        let info = inspect_preferring(&tarball, Some("cuda_core")).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("cuda-core"));
        assert_eq!(info.project_dir.as_deref(), Some("cuda_core"));
    }

    #[test]
    fn a_manifest_at_the_root_beats_a_matching_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n".as_slice(),
                ),
                (
                    "thing-1.0/examples/demo/pyproject.toml",
                    b"[project]\nname = \"demo\"\n".as_slice(),
                ),
            ],
        );
        let info = inspect_preferring(&tarball, Some("demo")).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("thing"));
        assert_eq!(info.project_dir, None);
        // One project at the root is not a monorepo, whatever sits below it.
        assert!(info.subprojects.is_empty());
    }

    /// A GitHub source tarball starts with a `pax_global_header` entry at the
    /// archive root. Three real packages (cuda-python, torchcodec, triton)
    /// shipped `%autosetup -n pax_global_header` because of it.
    #[test]
    fn pax_global_header_is_not_the_top_level_dir() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        {
            let file = std::fs::File::create(&tarball).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut ar = tar::Builder::new(enc);

            let mut add = |name: &str, body: &[u8]| {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                ar.append_data(&mut header, name, body).unwrap();
            };
            add("pax_global_header", b"52 comment=deadbeef\n");
            add("triton-3.5.0/setup.py", b"from setuptools import setup\n");

            ar.into_inner().unwrap().finish().unwrap();
        }

        let info = inspect(&tarball).unwrap();
        assert_eq!(info.top_level_dir.as_deref(), Some("triton-3.5.0"));
    }

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

    // --- Archive-format dispatch ------------------------------------------
    //
    // Each format gets a tiny single-file fixture, extracted into a tempdir,
    // and the extracted file's contents are checked byte-for-byte. The
    // fixtures intentionally use the same file name across formats so a
    // single `expect_extracted` helper verifies everything after extraction.

    fn write_tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            for (name, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                ar.append_data(&mut header, name, *body).unwrap();
            }
            ar.finish().unwrap();
        }
        buf
    }

    fn write_tar_bz2(path: &Path, entries: &[(&str, &[u8])]) {
        let tar_bytes = write_tar_bytes(entries);
        let file = std::fs::File::create(path).unwrap();
        let mut enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::fast());
        std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
        enc.finish().unwrap();
    }

    fn write_tar_zst(path: &Path, entries: &[(&str, &[u8])]) {
        let tar_bytes = write_tar_bytes(entries);
        let file = std::fs::File::create(path).unwrap();
        let mut enc = zstd::Encoder::new(file, 3).unwrap();
        std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
        enc.finish().unwrap();
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(body).unwrap();
        }
        zw.finish().unwrap();
    }

    /// Every extraction test shares the same fixture: a one-file archive
    /// whose extracted path and body are known in advance.
    fn make_fixture_entries() -> &'static [(&'static str, &'static [u8])] {
        &[("thing-1.0/README", b"hello\n")]
    }

    fn assert_extracted(dest: &Path) {
        let p = dest.join("thing-1.0").join("README");
        let body = std::fs::read(&p).expect("extracted file exists");
        assert_eq!(body, b"hello\n");
    }

    #[test]
    fn extract_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.gz");
        write_tar(&src, make_fixture_entries());
        let out = dir.path().join("out");
        extract(&src, &out).unwrap();
        assert_extracted(&out);
    }

    #[test]
    fn extract_skips_links_that_leave_the_root() {
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            for (name, target) in [
                ("pkg/abs", "/etc/passwd"),
                ("pkg/up", "../../outside"),
                ("pkg/ok", "file"),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o777);
                ar.append_link(&mut h, name, target).unwrap();
            }
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, "pkg/file", b"x".as_slice()).unwrap();
            ar.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let tgz = dir.path().join("a.tar.gz");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut enc, &buf).unwrap();
        std::fs::write(&tgz, enc.finish().unwrap()).unwrap();
        let dest = dir.path().join("out");
        extract(&tgz, &dest).unwrap();
        assert!(std::fs::symlink_metadata(dest.join("pkg/abs")).is_err());
        assert!(std::fs::symlink_metadata(dest.join("pkg/up")).is_err());
        assert!(std::fs::symlink_metadata(dest.join("pkg/ok")).is_ok());
    }

    #[test]
    fn extract_tar_bz2() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.bz2");
        write_tar_bz2(&src, make_fixture_entries());
        let out = dir.path().join("out");
        extract(&src, &out).unwrap();
        assert_extracted(&out);
    }

    #[test]
    fn extract_tar_zst() {
        // The Rust ap_service emits .tar.zst — this is the format the review
        // specifically flagged as needing extraction support.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.zst");
        write_tar_zst(&src, make_fixture_entries());
        let out = dir.path().join("out");
        extract(&src, &out).unwrap();
        assert_extracted(&out);
    }

    #[test]
    fn extract_zip_sdist() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.zip");
        write_zip(&src, make_fixture_entries());
        let out = dir.path().join("out");
        extract(&src, &out).unwrap();
        assert_extracted(&out);
    }

    #[test]
    fn extract_wheel_still_works() {
        // Same container as .zip, but with the `.whl` role — must take the
        // same code path and continue to extract without the system `unzip`
        // binary (which the old implementation shelled out to).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("foo-1.0-py3-none-any.whl");
        write_zip(&src, &[("foo-1.0.dist-info/METADATA", b"Name: foo\n")]);
        let out = dir.path().join("out");
        extract(&src, &out).unwrap();
        let body = std::fs::read(out.join("foo-1.0.dist-info/METADATA")).unwrap();
        assert_eq!(body, b"Name: foo\n");
    }

    #[test]
    fn extract_refuses_unrecognised_extension() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("thing.rar");
        std::fs::write(&src, b"not an archive").unwrap();
        let out = dir.path().join("out");
        let err = extract(&src, &out).expect_err("rar is not supported");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
    }

    // --- Inspection across formats ----------------------------------------

    #[test]
    fn inspect_tar_zst() {
        // The real motivation: a .tar.zst used to be fed to GzDecoder and
        // inspection reported no manifests, so an ap_service tarball looked
        // like an empty archive.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.zst");
        write_tar_zst(
            &src,
            &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
        );
        let info = inspect(&src).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("thing"));
        assert!(info.python_packaging);
    }

    #[test]
    fn inspect_tar_bz2() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.bz2");
        write_tar_bz2(
            &src,
            &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
        );
        let info = inspect(&src).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("thing"));
    }

    #[test]
    fn inspect_zip_sdist() {
        // A .zip sdist is a legacy shape that PyPI still accepts — inspection
        // has to look inside it the same way it does for tarballs.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.zip");
        write_zip(
            &src,
            &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
        );
        let info = inspect(&src).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("thing"));
        assert!(info.python_packaging);
    }

    #[test]
    fn non_utf8_manifest_does_not_fail_inspect() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.gz");
        write_tar(
            &src,
            &[
                (
                    "thing-1.0/pyproject.toml",
                    b"[project]\nname = \"thing\"\n# \xff\xfe\n".as_slice(),
                ),
                ("thing-1.0/vendor/Cargo.toml", b"\xff\xff".as_slice()),
            ],
        );
        let info = inspect(&src).unwrap();
        assert_eq!(info.project_name.as_deref(), Some("thing"));
    }

    #[test]
    fn malformed_poetry_table_does_not_fail_parse() {
        let raw = "[tool.poetry]\nname = 5\nversion = [1]\n[tool.other]\nx = 1\n";
        let p: PyprojectToml = toml::from_str(raw).unwrap();
        assert_eq!(p.declared_name(), None);
        let raw = "[tool.poetry]\nname = \"pp\"\n";
        let p: PyprojectToml = toml::from_str(raw).unwrap();
        assert_eq!(p.declared_name().as_deref(), Some("pp"));
    }

    #[test]
    fn copy_entry_is_bounded_by_budget_not_declared_size() {
        // The header claims 5 bytes but the stream holds 100, with only 10
        // bytes of total budget left: the copy must stop and fail.
        let mut b = Budget {
            bytes: MAX_TOTAL_BYTES - 10,
            ..Default::default()
        };
        b.charge(5).unwrap();
        let mut sink = Vec::new();
        let err = b
            .copy_entry(5, &mut std::io::repeat(b'x').take(100), &mut sink)
            .unwrap_err();
        assert!(matches!(err, Error::LimitExceeded(_)));
        assert!(sink.len() <= 11);
        // An honest entry is charged once, not twice.
        let mut b = Budget::default();
        b.charge(5).unwrap();
        b.copy_entry(5, &mut std::io::repeat(b'x').take(5), &mut Vec::new())
            .unwrap();
        assert_eq!(b.bytes, 5);
    }

    #[test]
    fn inspect_enforces_entry_limit() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("many.zip");
        let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("t-1/f{i}")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        write_zip(&src, &entries);
        assert!(matches!(inspect(&src), Err(Error::LimitExceeded(_))));
    }

    #[test]
    fn root_cmake_beats_nested_setup_py() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.gz");
        write_tar(
            &src,
            &[
                ("lib-1.0/CMakeLists.txt", b"project(lib)\n"),
                (
                    "lib-1.0/bindings/python/setup.py",
                    b"from setuptools import setup\n",
                ),
                (
                    "lib-1.0/vendor/dep/Cargo.toml",
                    b"[package]\nname = \"dep\"\n",
                ),
            ],
        );
        let info = inspect(&src).unwrap();
        assert!(info.cmake_manifest);
        assert!(!info.python_packaging);
        assert!(!info.cargo_manifest);
    }

    #[test]
    fn invalid_pyproject_does_not_fail_inspect() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.gz");
        write_tar(
            &src,
            &[
                ("thing-1.0/pyproject.toml", b"[project\nname = = \n"),
                ("thing-1.0/setup.py", b"from setuptools import setup\n"),
            ],
        );
        let info = inspect(&src).unwrap();
        assert!(info.python_packaging);
        assert_eq!(info.project_name, None);
        assert_eq!(
            info.build_system.backend.as_deref(),
            Some("setuptools.build_meta")
        );
    }

    #[test]
    fn dot_slash_prefix_does_not_become_top_level_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.zip");
        write_zip(
            &src,
            &[(
                "./thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n",
            )],
        );
        let info = inspect(&src).unwrap();
        assert_eq!(info.top_level_dir.as_deref(), Some("thing-1.0"));
        assert_eq!(info.files, vec!["thing-1.0/pyproject.toml".to_string()]);
        assert_eq!(info.project_name.as_deref(), Some("thing"));
    }

    #[test]
    fn budget_rejects_oversized_entries() {
        let mut b = Budget::default();
        assert!(b.charge(10).is_ok());
        assert!(matches!(
            b.charge(MAX_FILE_BYTES + 1),
            Err(Error::LimitExceeded(_))
        ));
        let mut b = Budget {
            entries: MAX_ENTRIES,
            ..Default::default()
        };
        assert!(matches!(b.charge(1), Err(Error::LimitExceeded(_))));
    }

    #[cfg(unix)]
    #[test]
    fn zip_extract_keeps_exec_bit_and_skips_symlinks() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let z = dir.path().join("a.zip");
        let file = std::fs::File::create(&z).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("run.sh", o.unix_permissions(0o755)).unwrap();
        zw.write_all(b"#!/bin/sh\n").unwrap();
        zw.add_symlink("link", "/etc/passwd", o).unwrap();
        zw.finish().unwrap();
        let dest = dir.path().join("out");
        extract(&z, &dest).unwrap();
        let mode = std::fs::metadata(dest.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        assert!(std::fs::symlink_metadata(dest.join("link")).is_err());
    }

    #[test]
    fn tar_error_display_includes_cause() {
        let e = Error::Tar(std::io::Error::other("boom"));
        assert!(e.to_string().contains("boom"));
    }

    #[test]
    fn wheel_root_is_purelib_reads_the_wheel_metadata() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        for (value, expect) in [("true", true), ("false", false)] {
            let path = dir.path().join(format!("w-{value}.whl"));
            let file = std::fs::File::create(&path).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            zw.start_file("pkg-1.0.dist-info/WHEEL", opts).unwrap();
            write!(zw, "Wheel-Version: 1.0\nRoot-Is-Purelib: {value}\nTag: py3-none-manylinux_2_17_x86_64\n").unwrap();
            zw.finish().unwrap();
            assert_eq!(wheel_root_is_purelib(&path), Some(expect));
        }
        assert_eq!(wheel_root_is_purelib(&dir.path().join("missing.whl")), None);
    }

    fn write_wheel(path: &Path, files: &[&str]) {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        for f in files {
            zw.start_file(*f, opts).unwrap();
            zw.write_all(b"x").unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn wheel_layout_finds_an_importable_package() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("a.whl");
        write_wheel(
            &w,
            &["pkg/__init__.py", "pkg/x.py", "pkg-1.0.dist-info/RECORD"],
        );
        let l = wheel_layout(&w).unwrap();
        assert_eq!(l.importable.as_deref(), Some("pkg"));
        assert_eq!(l.first_shared_lib, None);
    }

    /// NVIDIA's shape: a PEP 420 namespace with only shared libraries — nothing
    /// to import, so the check must look at the file instead.
    #[test]
    fn wheel_layout_of_a_namespace_only_wheel_offers_a_file_to_check() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("n.whl");
        write_wheel(
            &w,
            &[
                "nvidia/cu13/lib/libnvJitLink.so.13",
                "nvidia/cu13/lib/deep/libother.so.1",
                "nvidia/cu13/include/nvJitLink.h",
                "nvidia_nvjitlink-1.0.dist-info/RECORD",
            ],
        );
        let l = wheel_layout(&w).unwrap();
        assert_eq!(l.importable, None);
        assert_eq!(
            l.first_shared_lib.as_deref(),
            Some("nvidia/cu13/lib/libnvJitLink.so.13")
        );
        assert_eq!(l.header_dirs, vec!["nvidia/cu13/include".to_string()]);
    }

    /// Multiple headers in the same directory collapse to one entry;
    /// headers in distinct directories (and a `.hpp` alongside a `.h`) each
    /// get their own, sorted. A `.dist-info` header (unlikely, but
    /// hypothetically possible metadata) must not count.
    #[test]
    fn wheel_layout_dedupes_and_sorts_header_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("h.whl");
        write_wheel(
            &w,
            &[
                "nvidia/nccl/include/nccl.h",
                "nvidia/nccl/include/nccl_device.h",
                "nvidia/cublas/include/cublas_v2.hpp",
                "nvidia_nccl-1.0.dist-info/licenses/some.h",
            ],
        );
        let l = wheel_layout(&w).unwrap();
        assert_eq!(
            l.header_dirs,
            vec![
                "nvidia/cublas/include".to_string(),
                "nvidia/nccl/include".to_string(),
            ]
        );
    }

    /// The real nvidia-nccl-cu13 shape: headers nested many levels under one
    /// top-level `include/` (`include/nccl_device/gin/gdaki/...`). Only the
    /// shallowest directory should be kept — rpmlint's `addFilter` is an
    /// unanchored substring match, so a filter on the parent already covers
    /// every descendant, and one redundant filter per nested directory would
    /// just be noise.
    #[test]
    fn wheel_layout_collapses_nested_header_dirs_to_the_shallowest_one() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("nested.whl");
        write_wheel(
            &w,
            &[
                "nvidia/nccl/include/nccl.h",
                "nvidia/nccl/include/nccl_device/comm.h",
                "nvidia/nccl/include/nccl_device/gin/gdaki/common.h",
            ],
        );
        let l = wheel_layout(&w).unwrap();
        assert_eq!(l.header_dirs, vec!["nvidia/nccl/include".to_string()]);
    }

    #[test]
    fn wheel_layout_accepts_a_single_module_wheel() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("m.whl");
        write_wheel(&w, &["six.py", "six-1.0.dist-info/METADATA"]);
        assert_eq!(wheel_layout(&w).unwrap().importable.as_deref(), Some("six"));
    }
}
