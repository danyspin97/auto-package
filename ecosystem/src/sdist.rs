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
use std::path::{Component, Path, PathBuf};

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
    /// An `.tar.xz`: NVIDIA's redistributable archives.
    TarXz,
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
    } else if s.ends_with(".tar.xz") || s.ends_with(".txz") {
        Some(ArchiveKind::TarXz)
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
        ArchiveKind::TarXz => Box::new(lzma_rust2::XzReader::new(file, true)),
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
}

/// Extraction caps: a hostile or corrupt archive must not exhaust the disk
/// or the inode table. Generous enough for any real sdist or CUDA wheel.
const MAX_ENTRIES: usize = 200_000;
const MAX_TOTAL_BYTES: u64 = 4 << 30;
const MAX_FILE_BYTES: u64 = 2 << 30;
/// Cap on a manifest read during inspection.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// Largest unpacked-to-compressed ratio accepted, past a fixed allowance: gzip
/// can expand about 1000:1, so a small upload can ask for gigabytes of disk.
const MAX_EXPANSION: u64 = 200;
const EXPANSION_ALLOWANCE: u64 = 64 << 20;

/// Running totals checked against the extraction caps.
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
    /// Size of the archive on disk; 0 means unknown and turns the ratio check off.
    compressed: u64,
}

impl Budget {
    /// A budget for reading `archive`, whose size on disk bounds how much it
    /// may expand to.
    fn for_file(archive: &Path) -> Self {
        Budget {
            compressed: std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0),
            ..Default::default()
        }
    }

    /// The expansion-ratio cap: more than [`MAX_EXPANSION`] times the archive's
    /// size (after [`EXPANSION_ALLOWANCE`]) is a decompression bomb.
    fn check_ratio(&self) -> Result<(), Error> {
        if self.compressed > 0
            && self.bytes
                > self
                    .compressed
                    .saturating_mul(MAX_EXPANSION)
                    .saturating_add(EXPANSION_ALLOWANCE)
        {
            return Err(Error::LimitExceeded(
                "unpacked size is far larger than the archive (possible decompression bomb)"
                    .to_string(),
            ));
        }
        Ok(())
    }

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
        self.check_ratio()
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
        self.check_ratio()
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
    /// For a wheel with no top-level `__init__.py` or module (a PEP 420
    /// namespace: `cuda/bindings/__init__.py`, `nccl/ep/__init__.py`): the dotted
    /// name of its shallowest regular package (`cuda.bindings`). Only its parents
    /// are namespaces, so `importlib.util.find_spec` on it runs no package code.
    pub namespace_module: Option<String>,
    /// The SPDX `License-Expression:` of the wheel's own METADATA, for a
    /// release whose PyPI record carries no usable licence.
    pub license_expression: Option<String>,
    /// The free-text `License:` header of the METADATA (first line, bounded),
    /// and its `License ::` trove classifiers: the licence of a wheel built
    /// before `License-Expression` existed.
    pub license_text: Option<String>,
    pub license_classifiers: Vec<String>,
    /// The first prose paragraph of the METADATA's long description (the
    /// message body, or a `Description:` header), on one line and bounded:
    /// what the generated `%description` says instead of repeating the Summary.
    pub description: Option<String>,
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
    /// Every file of the wheel (no directory entries), as stored.
    pub files: Vec<String>,
    /// The `<name>-<version>.dist-info` directory.
    pub dist_info: Option<String>,
    /// The `Requires-Dist` lines of the wheel's `METADATA`, verbatim.
    pub requires_dist: Vec<String>,
    /// Script names from the `[console_scripts]`/`[gui_scripts]` sections of
    /// the wheel's `*.dist-info/entry_points.txt`: pip generates one
    /// `/usr/bin/<name>` wrapper for each, which a spec that lists only
    /// site-packages leaves unpackaged (python-triton's `proton`, python-
    /// cuda-tile's `cutile-cache`). Names that are not a plain file name are
    /// dropped — they end up in a generated spec. In file order, deduplicated.
    pub console_scripts: Vec<String>,
    /// The wheel bundles auditwheel-style libraries: a `<name>.libs/`
    /// directory, or a shared library whose name carries the 8-hex-digit hash
    /// auditwheel appends (`libavcodec-f34387fd.so.62.11.100`). The RPM's
    /// auto-generated `Requires` then name those hashed sonames, which nothing
    /// provides (the matching `Provides` are not exported), so the package
    /// cannot be installed unless the spec excludes them.
    pub has_hashed_libs: bool,
    /// A bundled shared library carries a `DT_RUNPATH`/`DT_RPATH` containing
    /// `$ORIGIN` (vendor libraries find their siblings that way). rpmlint's
    /// `binary-or-shlib-defines-rpath` is badness 10000 for it, so one hit
    /// aborts the check — and, since an `rpmlintrc` filter nothing matches is
    /// itself an error (`unused-rpmlintrc-filter`), the filter may only be
    /// written when this is true.
    pub origin_runpath: bool,
    /// Zero-byte files the wheel ships (relative to site-packages, sorted):
    /// upstream's placeholder `custom.css`/`loader.js`. rpmlint's
    /// `zero-length` is an error for them, they cannot be made non-empty
    /// without changing the payload, so the spec filters exactly these. Not
    /// listed: what rpmlint itself accepts empty (`__init__.py`, `py.typed`,
    /// `.nosearch`) — a filter nothing matches is `unused-rpmlintrc-filter`.
    pub empty_files: Vec<String>,
}

impl WheelLayout {
    /// The wheel only carries shared libraries: nothing to `import`. NVIDIA's
    /// `nvidia-<lib>-cuNN` wheels are this, a vendor's C/CUDA library shipped
    /// through PyPI.
    pub fn is_library_only(&self) -> bool {
        self.importable.is_none() && self.first_shared_lib.is_some()
    }
}

/// Whether `file` (a file name or a soname, without any `(...)` suffix) is a
/// shared library named `lib<stem>-<8 hex digits><rest>.so<rest>` the way
/// auditwheel renames a vendored library. The hash must include a letter, so a
/// date-stamped `libfoo-20240101.so.1` is not taken for one. The single
/// definition of "hashed": [`hashed_soname_requires_exclude`] is the same rule
/// as an rpm regex, and `depclass` asks this function too.
pub fn is_hashed_soname(file: &str) -> bool {
    if !file.starts_with("lib") {
        return false;
    }
    file.match_indices('-').any(|(i, _)| {
        let Some(hash) = file.as_bytes().get(i + 1..i + 9) else {
            return false;
        };
        if !hash.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            || !hash.iter().any(u8::is_ascii_lowercase)
        {
            return false;
        }
        let tail = &file[i + 9..];
        tail.starts_with('.') && (tail.ends_with(".so") || tail.contains(".so."))
    })
}

/// The `__requires_exclude` regex that matches exactly the sonames
/// [`is_hashed_soname`] accepts, as rpm matches it against a Requires name
/// (`libfoo-1a2b3c4d.so.1()(64bit)`). rpm's regex has no lookahead, so "the
/// hash has a letter" is spelled out as the position of its first letter.
pub fn hashed_soname_requires_exclude() -> &'static str {
    static RE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        let hash: Vec<String> = (0..8)
            .map(|k| {
                let digits = if k == 0 {
                    String::new()
                } else {
                    format!("[0-9]{{{k}}}")
                };
                let rest = if k == 7 {
                    String::new()
                } else {
                    format!("[0-9a-f]{{{}}}", 7 - k)
                };
                format!("{digits}[a-f]{rest}")
            })
            .collect();
        format!(r"^lib.*-({})(\..*)?\.so([.(].*)?$", hash.join("|"))
    })
}

/// Script names declared in an `entry_points.txt` body (see
/// [`WheelLayout::console_scripts`]). INI-shaped: `[section]` headers, then
/// `name = module:func [extras]` lines; `#`/`;` start a comment.
fn console_scripts_from_entry_points(text: &str) -> Vec<String> {
    let mut scripts: Vec<String> = Vec::new();
    let mut in_scripts = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_scripts = matches!(section.trim(), "console_scripts" | "gui_scripts");
            continue;
        }
        if !in_scripts {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let plain = !name.is_empty()
            && !name.starts_with(['-', '.'])
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'));
        if plain && !scripts.iter().any(|s| s == name) {
            scripts.push(name.to_string());
        }
    }
    scripts
}

/// Most bytes read from one shared library to look at its dynamic section:
/// `.dynamic` and `.dynstr` sit near the start of an ELF file, far before
/// the code of a library that can be hundreds of megabytes.
const MAX_ELF_BYTES: u64 = 16 * 1024 * 1024;
/// Most shared libraries of one wheel looked at for an `$ORIGIN` RUNPATH.
const MAX_ELF_FILES: usize = 256;

/// The leading bytes of a stream, read on demand and never past
/// [`MAX_ELF_BYTES`], so a parser can ask for any range without the whole
/// (possibly compressed, so not seekable) file being read for it.
struct Prefix<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: Read> Prefix<R> {
    /// Up to `len` bytes at `off`: fewer when the stream ends first, `None`
    /// when the range starts past its end or past the cap.
    fn upto(&mut self, off: u64, len: u64) -> Option<&[u8]> {
        let end = off.checked_add(len)?.min(MAX_ELF_BYTES);
        let (off, end) = (usize::try_from(off).ok()?, usize::try_from(end).ok()?);
        if let Some(missing) = end.checked_sub(self.buf.len()) {
            (&mut self.reader)
                .take(missing as u64)
                .read_to_end(&mut self.buf)
                .ok()?;
        }
        let end = end.min(self.buf.len());
        self.buf.get(off..end)
    }

    /// Exactly `len` bytes at `off`, or `None`.
    fn exact(&mut self, off: u64, len: u64) -> Option<&[u8]> {
        self.upto(off, len).filter(|b| b.len() as u64 == len)
    }
}

fn le_u16(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le_u32(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        b.get(at..at + 4)?.try_into().ok()?,
    )))
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Whether an ELF64 little-endian file has a `DT_RUNPATH` or `DT_RPATH`
/// containing `$ORIGIN`. Reads the program headers, the `PT_DYNAMIC` array and
/// the string table it points at, every range bounds-checked and capped;
/// anything unexpected (not ELF, 32-bit, big-endian, truncated, a bad offset)
/// is `false`.
fn elf_has_origin_runpath(reader: impl Read) -> bool {
    elf_origin_runpath(&mut Prefix {
        reader,
        buf: Vec::new(),
    })
    .unwrap_or(false)
}

fn elf_origin_runpath<R: Read>(file: &mut Prefix<R>) -> Option<bool> {
    const PT_LOAD: u64 = 1;
    const PT_DYNAMIC: u64 = 2;
    const PHDR_MIN: u64 = 56;
    const DT_STRTAB: u64 = 5;
    const DT_STRSZ: u64 = 10;
    const DT_RPATH: u64 = 15;
    const DT_RUNPATH: u64 = 29;
    // A library has a handful of program headers and one `.dynamic` of a few
    // dozen entries; these bounds are far above that.
    const MAX_PHNUM: u64 = 256;
    const MAX_DYNAMIC_BYTES: u64 = 64 * 1024;
    const MAX_RUNPATH_BYTES: u64 = 4096;

    let header = file.exact(0, 64)?;
    // ELF magic, ELFCLASS64, ELFDATA2LSB.
    if header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let (phoff, phentsize, phnum) = (
        le_u64(header, 32)?,
        le_u16(header, 54)?,
        le_u16(header, 56)?,
    );
    if phentsize < PHDR_MIN || phnum == 0 || phnum > MAX_PHNUM {
        return None;
    }
    let phdrs = file.exact(phoff, phentsize * phnum)?;
    // (file offset, virtual address, size in file) per segment.
    let mut loads: Vec<(u64, u64, u64)> = Vec::new();
    let mut dynamic = None;
    for ph in phdrs.chunks_exact(phentsize as usize) {
        let segment = (le_u64(ph, 8)?, le_u64(ph, 16)?, le_u64(ph, 32)?);
        match le_u32(ph, 0)? {
            PT_LOAD => loads.push(segment),
            PT_DYNAMIC => dynamic = Some(segment),
            _ => {}
        }
    }
    let (dyn_off, _, dyn_size) = dynamic?;
    let entries = file.upto(dyn_off, dyn_size.min(MAX_DYNAMIC_BYTES))?;
    let (mut strtab, mut strsz, mut runpaths) = (None, None, Vec::new());
    for entry in entries.chunks_exact(16) {
        let (tag, val) = (le_u64(entry, 0)?, le_u64(entry, 8)?);
        match tag {
            0 => break,
            DT_STRTAB => strtab = Some(val),
            DT_STRSZ => strsz = Some(val),
            DT_RPATH | DT_RUNPATH => runpaths.push(val),
            _ => {}
        }
    }
    if runpaths.is_empty() {
        return Some(false);
    }
    // `DT_STRTAB` is a virtual address: find the segment holding it.
    let strtab = strtab?;
    let strtab_off = loads.iter().find_map(|&(off, vaddr, size)| {
        let rel = strtab.checked_sub(vaddr).filter(|rel| *rel < size)?;
        off.checked_add(rel)
    })?;
    for val in runpaths {
        // The string lies inside the table when its size is known.
        let room = strsz.map_or(MAX_RUNPATH_BYTES, |sz| sz.saturating_sub(val));
        let at = strtab_off.checked_add(val)?;
        let Some(bytes) = file.upto(at, room.min(MAX_RUNPATH_BYTES)) else {
            continue;
        };
        let bytes = bytes.split(|b| *b == 0).next().unwrap_or(&[]);
        if bytes.windows(7).any(|w| w == b"$ORIGIN") {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether any shared library among `names` of the zip has an `$ORIGIN`
/// RUNPATH/RPATH (see [`WheelLayout::origin_runpath`]). Unreadable entries
/// count as not having one.
fn any_origin_runpath<R: Read + std::io::Seek>(
    zip_archive: &mut zip::ZipArchive<R>,
    names: &[&str],
) -> bool {
    names.iter().take(MAX_ELF_FILES).any(|name| {
        zip_archive
            .by_name(name)
            .is_ok_and(|entry| entry.size() >= 64 && elf_has_origin_runpath(entry))
    })
}

/// The shallowest regular package of a wheel that has none at its top level: the
/// directory with an `__init__.py` closest to the root, as a dotted name
/// (`cuda/bindings/__init__.py` is `cuda.bindings`). Every component must be a
/// plain identifier, and test directories are not offered.
fn namespace_module_of(names: &[String], skip: &dyn Fn(&str) -> bool) -> Option<String> {
    let is_ident = |c: &str| {
        !c.is_empty()
            && !c.starts_with(|ch: char| ch.is_ascii_digit())
            && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    };
    names
        .iter()
        .filter_map(|n| n.strip_suffix("/__init__.py"))
        .filter(|dir| dir.contains('/') && !skip(dir.split('/').next().unwrap_or("")))
        .filter(|dir| {
            dir.split('/')
                .all(|c| is_ident(c) && !matches!(c, "tests" | "test" | "testing"))
        })
        .min_by_key(|dir| (dir.matches('/').count(), dir.len()))
        .map(|dir| dir.replace('/', "."))
}

/// Whether a wheel's file names show auditwheel-bundled libraries: a
/// `<name>.libs/` directory or a hash-suffixed `.so`.
fn names_have_hashed_libs(names: &[String]) -> bool {
    names.iter().any(|n| {
        let top = n.split('/').next().unwrap_or("");
        let file = n.rsplit('/').next().unwrap_or("");
        let skipped = top.ends_with(".dist-info") || top.ends_with(".data") || top == "__pycache__";
        !skipped && n.contains('/') && (top.ends_with(".libs") || is_hashed_soname(file))
    })
}

/// The file names (no directories) a wheel unpacks, read from the zip's
/// directory alone: no metadata is parsed and no library opened, unlike
/// [`wheel_layout`], so it is cheap enough to ask of every sibling wheel of a
/// project (an NVIDIA wheel is hundreds of megabytes). `None` when the file is
/// not a readable zip.
pub fn wheel_file_names(wheel: &Path) -> Option<Vec<String>> {
    let zip_archive = zip::ZipArchive::new(std::fs::File::open(wheel).ok()?).ok()?;
    let mut names: Vec<String> = zip_archive
        .file_names()
        .filter(|n| !n.ends_with('/'))
        .map(str::to_string)
        .collect();
    names.sort();
    Some(names)
}

/// [`WheelLayout::has_hashed_libs`] alone, from the zip's file names only: no
/// metadata is read and no library opened, so it is cheap enough to ask of
/// every wheel in a project.
pub fn wheel_has_hashed_libs(wheel: &Path) -> bool {
    let Ok(file) = std::fs::File::open(wheel) else {
        return false;
    };
    let Ok(zip_archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    let names: Vec<String> = zip_archive.file_names().map(str::to_string).collect();
    names_have_hashed_libs(&names)
}

/// Inspect a wheel's file list. `None` when it cannot be read as a zip.
pub fn wheel_layout(wheel: &Path) -> Option<WheelLayout> {
    let file = std::fs::File::open(wheel).ok()?;
    let mut zip_archive = zip::ZipArchive::new(file).ok()?;
    let names: Vec<String> = zip_archive.file_names().map(str::to_string).collect();
    let dist_info = names
        .iter()
        .filter_map(|n| n.split('/').next())
        .find(|top| top.ends_with(".dist-info"))
        .map(str::to_string);
    let metadata = dist_info.as_ref().and_then(|d| {
        let mut entry = zip_archive.by_name(&format!("{d}/METADATA")).ok()?;
        read_manifest(&mut entry)
    });
    // Core metadata is RFC 822: headers end at the first blank line.
    let headers = |name: &'static str| -> Vec<String> {
        metadata
            .iter()
            .flat_map(|text| text.lines().take_while(|l| !l.trim().is_empty()))
            .filter_map(move |l| l.strip_prefix(name))
            .map(|v| v.trim().to_string())
            .collect()
    };
    let requires_dist = headers("Requires-Dist:");
    let license_expression = headers("License-Expression:")
        .into_iter()
        .find(|v| !v.is_empty());
    let license_text = headers("License:")
        .into_iter()
        .map(|v| v.lines().next().unwrap_or("").trim().to_string())
        .find(|v| !v.is_empty() && !v.eq_ignore_ascii_case("unknown") && v.len() <= 200);
    let license_classifiers: Vec<String> = headers("Classifier:")
        .into_iter()
        .filter(|c| c.starts_with("License ::"))
        .collect();
    let description = metadata.as_deref().and_then(metadata_description);
    let files: Vec<String> = names
        .iter()
        .filter(|n| !n.ends_with('/'))
        .cloned()
        .collect();
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
    let namespace_module = if importable.is_none() {
        namespace_module_of(&names, &skip)
    } else {
        None
    };
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
    // An unreadable `entry_points.txt` just means no scripts are reported.
    let console_scripts = dist_info
        .as_ref()
        .and_then(|d| {
            let mut entry = zip_archive.by_name(&format!("{d}/entry_points.txt")).ok()?;
            read_manifest(&mut entry)
        })
        .map(|text| console_scripts_from_entry_points(&text))
        .unwrap_or_default();
    let has_hashed_libs = names_have_hashed_libs(&names);
    let shared_libs: Vec<&str> = names
        .iter()
        .filter(|n| {
            let file = n.rsplit('/').next().unwrap_or("");
            !n.ends_with('/')
                && !skip(&top_of(n))
                && (file.ends_with(".so") || file.contains(".so."))
        })
        .map(String::as_str)
        .collect();
    let origin_runpath = any_origin_runpath(&mut zip_archive, &shared_libs);
    let mut empty_files: Vec<String> = (0..zip_archive.len())
        .filter_map(|i| {
            let entry = zip_archive.by_index_raw(i).ok()?;
            let name = entry.name().to_string();
            (!entry.is_dir()
                && entry.size() == 0
                && !skip(&top_of(&name))
                && !rpmlint_accepts_empty(&name))
            .then_some(name)
        })
        .collect();
    empty_files.sort_unstable();
    Some(WheelLayout {
        importable,
        namespace_module,
        license_expression,
        license_text,
        license_classifiers,
        description,
        first_shared_lib,
        header_dirs,
        files,
        dist_info,
        requires_dist,
        console_scripts,
        has_hashed_libs,
        origin_runpath,
        empty_files,
    })
}

/// The first prose paragraph of a METADATA's long description (the body after
/// the header block), joined on one line, at most 600 characters cut at a word.
/// Headings, badge/image lines, HTML, code fences and tables are skipped: a
/// paragraph is prose only when none of its lines starts like markup. `None`
/// when nothing qualifies or the text carries a `TODO`/`FIXME` word.
fn metadata_description(metadata: &str) -> Option<String> {
    let body = metadata
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or("");
    let mut in_fence = false;
    let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if t.is_empty() {
            if !paragraphs.last().is_some_and(Vec::is_empty) {
                paragraphs.push(Vec::new());
            }
        } else if let Some(p) = paragraphs.last_mut() {
            p.push(t);
        }
    }
    let markup = |l: &str| {
        l.starts_with([
            '#', '<', '|', '>', '!', '=', '`', '*', '-', '+', '.', ':', '[',
        ]) || l.starts_with("http")
            || l.starts_with(|c: char| c.is_ascii_digit()) && l.contains(". ")
    };
    let para = paragraphs.iter().find(|p| {
        !p.is_empty() && !p.iter().any(|l| markup(l)) && p.join(" ").split_whitespace().count() >= 5
    })?;
    let mut text = para.join(" ");
    if text.contains("TODO") || text.contains("FIXME") {
        return None;
    }
    if text.chars().count() > 600 {
        let cut: String = text.chars().take(600).collect();
        let at = cut.rfind(". ").map(|i| i + 1).or_else(|| cut.rfind(' '))?;
        text = cut[..at].trim_end_matches([',', ';', ' ']).to_string();
        if !text.ends_with('.') {
            text.push('.');
        }
    }
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Files rpmlint's `zero-length` check lets be empty (its
/// `normal_zero_length_regex`).
fn rpmlint_accepts_empty(name: &str) -> bool {
    let file = name.rsplit('/').next().unwrap_or(name);
    matches!(file, "__init__.py" | "py.typed" | ".nosearch")
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
///
/// `dest` must be absent or empty. The archive is unpacked beside it and
/// renamed into place only once complete, so a failed or interrupted unpack
/// never leaves a `dest` that looks like a finished extraction.
pub fn extract(tarball: &Path, dest: &Path) -> Result<(), Error> {
    extract_where(tarball, dest, |_| true)
}

/// Refuse a symlink whose location or target can reach outside `root`.
///
/// Walks the link's directory and then its target one component at a time,
/// keeping the path relative to `root`. Every directory it steps through must
/// be a real directory, not a link, so the walk is exact and a `..` can only
/// pop a real directory. The final component of the target may be a link:
/// it was checked when it was created.
fn check_link(root: &Path, link: &Path, target: &Path) -> Result<(), Error> {
    let escape = || Error::LinkEscapes {
        link: link.to_path_buf(),
        target: target.to_path_buf(),
    };
    if target.has_root() {
        return Err(escape());
    }
    let is_link_on_disk = |rel: &Path| {
        std::fs::symlink_metadata(root.join(rel)).is_ok_and(|m| m.file_type().is_symlink())
    };

    let mut rel = std::path::PathBuf::new();
    for c in link.parent().unwrap_or(Path::new("")).components() {
        match c {
            Component::Normal(p) => {
                if is_link_on_disk(&rel.join(p)) {
                    return Err(escape());
                }
                rel.push(p);
            }
            Component::CurDir => {}
            _ => return Err(escape()),
        }
    }

    let parts: Vec<Component<'_>> = target.components().collect();
    for (i, c) in parts.iter().enumerate() {
        match c {
            Component::Normal(p) => {
                let last = i + 1 == parts.len();
                if !last && is_link_on_disk(&rel.join(p)) {
                    return Err(escape());
                }
                rel.push(p);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !rel.pop() {
                    return Err(escape());
                }
            }
            _ => return Err(escape()),
        }
    }
    Ok(())
}

/// [`extract`] restricted to the entries `keep` accepts. The path it is given
/// is the entry's path inside the archive (including the top-level directory).
/// Skipped entries are still decompressed on the way past, but never written.
pub fn extract_where(
    tarball: &Path,
    dest: &Path,
    keep: impl Fn(&Path) -> bool,
) -> Result<(), Error> {
    let mut partial = dest.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_dir_all(&partial);
    let result = extract_into(tarball, &partial, &keep).and_then(|()| {
        // Only an empty `dest` is replaced; anything else is an error here.
        let _ = std::fs::remove_dir(dest);
        std::fs::rename(&partial, dest).map_err(|e| Error::Io {
            path: dest.to_path_buf(),
            source: e,
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    result
}

fn extract_into(tarball: &Path, dest: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(), Error> {
    let kind = classify(tarball).ok_or_else(|| Error::Io {
        path: tarball.to_path_buf(),
        source: std::io::Error::other(format!(
            "unrecognised archive extension on {}: expected one of \
             .tar.gz/.tgz, .tar.bz2/.tbz2, .tar.zst/.tzst, .zip, .whl",
            tarball.display(),
        )),
    })?;
    match kind {
        ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarZstd | ArchiveKind::TarXz => {
            let mut archive = open_tar(tarball, kind)?;
            // Both are off by default; stated so a setuid bit or an xattr
            // from an untrusted archive can never be applied.
            archive.set_preserve_permissions(false);
            archive.set_unpack_xattrs(false);
            std::fs::create_dir_all(dest).map_err(|e| Error::Io {
                path: dest.to_path_buf(),
                source: e,
            })?;
            let mut budget = Budget::for_file(tarball);
            // `unpack_in` keeps tar-rs's traversal and symlink protections.
            for entry in archive.entries().map_err(Error::Tar)? {
                let mut entry = entry.map_err(Error::Tar)?;
                if !keep(&entry.path().map_err(Error::Tar)?) {
                    continue;
                }
                budget.charge(entry.size())?;
                if let Some(reason) = escaping_link(&entry) {
                    tracing::warn!("skipping tar entry: {reason}");
                    continue;
                }
                // Each symlink is also checked against what is already on
                // disk. `unpack_in` only canonicalizes the parent of the entry
                // it is writing, so a link planted earlier can redirect a
                // later one out of the tree even when each link looks fine
                // on its own.
                if entry.header().entry_type().is_symlink() {
                    let link = entry.path().map_err(Error::Tar)?.into_owned();
                    if let Some(target) = entry.link_name().map_err(Error::Tar)? {
                        check_link(dest, &link, &target)?;
                    }
                }
                // A hard link's target is relative to the archive root and
                // is opened through whatever directories are on disk, so a
                // symlinked directory planted earlier would redirect it.
                // Checked as a link at the root.
                if entry.header().entry_type().is_hard_link()
                    && let Some(target) = entry.link_name().map_err(Error::Tar)?
                {
                    let link = entry.path().map_err(Error::Tar)?.into_owned();
                    let at_root = link.file_name().map(Path::new).unwrap_or(&link);
                    check_link(dest, at_root, &target)?;
                }
                entry.unpack_in(dest).map_err(Error::Tar)?;
            }
            Ok(())
        }
        ArchiveKind::Zip | ArchiveKind::Wheel => extract_zip(tarball, dest, keep),
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
                Component::ParentDir if parts.pop().is_none() => return refuse(),
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
fn extract_zip(archive: &Path, dest: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(), Error> {
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
    let mut budget = Budget::for_file(archive);
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
        if !keep(&rel) {
            continue;
        }
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

/// The member paths of a tar archive (`.tar.gz`, `.tar.bz2`, `.tar.zst`,
/// `.tar.xz`) without unpacking it: directories end in `/`, links are listed
/// like files, a leading `./` is dropped and entries that are absolute or climb
/// out with `..` are skipped. Stops at the same entry cap as extraction.
pub fn list_archive_members(path: &Path) -> Result<Vec<String>, Error> {
    let kind = classify(path).filter(|k| !matches!(k, ArchiveKind::Zip | ArchiveKind::Wheel));
    let Some(kind) = kind else {
        return Err(Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other("not a tar archive"),
        });
    };
    let mut archive = open_tar(path, kind)?;
    let mut out = Vec::new();
    for entry in archive.entries().map_err(Error::Tar)? {
        let entry = entry.map_err(Error::Tar)?;
        if out.len() >= MAX_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "more than {MAX_ENTRIES} entries"
            )));
        }
        let entry_path = entry.path().map_err(Error::Tar)?.into_owned();
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            continue;
        }
        let mut name = entry_path.to_string_lossy().into_owned();
        while let Some(rest) = name.strip_prefix("./") {
            name = rest.to_string();
        }
        if name.is_empty() || name == "." {
            continue;
        }
        if entry.header().entry_type().is_dir() && !name.ends_with('/') {
            name.push('/');
        }
        out.push(name);
    }
    Ok(out)
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
        ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarZstd | ArchiveKind::TarXz => {
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
    let mut budget = Budget::for_file(path);
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
        // Python's tarfile writes directory entries without a trailing slash,
        // so the entry type is the reliable signal; the slash is a fallback.
        let is_dir = entry.header().entry_type().is_dir() || path_str.ends_with('/');
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
    let mut budget = Budget::for_file(path);
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
    let mut top_level_seen = false;
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
        // The name ends up unescaped in the spec's `%autosetup -n` line, so a
        // hostile archive must not get to pick it: an unsafe name is treated
        // as no top-level directory at all, and the first candidate is final.
        if !top_level_seen && components.len() >= 2 {
            top_level_seen = true;
            let name = components[0].as_os_str().to_string_lossy().into_owned();
            if is_safe_top_level_dir(&name) {
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
    let mut console_scripts = Vec::new();
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
                    console_scripts = doc.script_names();
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
        project_name = doc
            .package
            .and_then(|p| p.name)
            .filter(|n| is_plain_project_name(n));
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
        console_scripts,
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
    // Loose like `tool`: a malformed table must not fail the parse.
    scripts: Option<toml::Value>,
    #[serde(rename = "gui-scripts")]
    gui_scripts: Option<toml::Value>,
}

/// Whether an archive's top-level directory name can be written verbatim into
/// a spec (`%autosetup -n <name>`) and a shell command. Letters and digits in
/// any script are allowed (e.g. `Pyöpt-1.0`), plus `. _ - + ~`: no whitespace,
/// control character, newline, `%` (RPM macro), `/` or other shell or spec
/// metacharacters. Shared with `ap-build` so both sides accept the same names.
pub fn is_safe_top_level_dir(name: &str) -> bool {
    name.len() <= 255
        && !matches!(name, "" | "." | "..")
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~'))
}

/// Whether an upstream-declared project name is safe to build a package, a
/// directory and a command line around: the characters PyPI and crates.io
/// allow, so no path separator, whitespace, shell or XML metacharacter. The
/// manifest is untrusted, and the name ends up in file names and spec text.
fn is_plain_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

impl PyprojectToml {
    /// Keys of `[project.scripts]`, `[project.gui-scripts]` and the legacy
    /// `[tool.poetry.scripts]`, in order, without a name that is not a plain
    /// file name (it ends up in spec lines and paths).
    fn script_names(&self) -> Vec<String> {
        let mut tables: Vec<&toml::Value> = Vec::new();
        if let Some(p) = &self.project {
            tables.extend(p.scripts.iter().chain(p.gui_scripts.iter()));
        }
        if let Some(t) = self
            .tool
            .as_ref()
            .and_then(|t| t.get("poetry")?.get("scripts"))
        {
            tables.push(t);
        }
        let mut out: Vec<String> = Vec::new();
        for table in tables {
            let Some(table) = table.as_table() else {
                continue;
            };
            for name in table.keys() {
                let plain = !name.starts_with(['-', '.'])
                    && name.len() <= 100
                    && name.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+')
                    });
                if plain && !out.contains(name) {
                    out.push(name.clone());
                }
            }
        }
        out
    }

    /// `[project].name`, else the legacy `[tool.poetry].name`.
    fn declared_name(self) -> Option<String> {
        self.project
            .and_then(|p| p.name)
            .or_else(|| {
                self.tool?
                    .get("poetry")?
                    .get("name")?
                    .as_str()
                    .map(str::to_string)
            })
            .filter(|n| is_plain_project_name(n))
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

/// The project's source repository on GitHub or GitLab, from the metadata an
/// unpacked sdist carries: `Project-URL:` lines of `PKG-INFO` and
/// `[project.urls]` of `pyproject.toml`, then `Home-page`. Keys that name the
/// source (`Source`, `Repository`, `Code`) win over `Homepage`; a URL that is
/// not a repository on those two hosts (docs, an issue tracker on another
/// host) is skipped. The result is the canonical `https://host/owner/repo`.
pub fn source_repo_from_metadata(pkg_info: &str, pyproject: &str) -> Option<String> {
    let mut urls: Vec<(String, String)> = Vec::new();
    for line in pkg_info.lines() {
        // Metadata headers end at the first blank line; the body is the readme.
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Project-URL:") {
            if let Some((k, u)) = v.split_once(',') {
                urls.push((k.trim().to_string(), u.trim().to_string()));
            }
        } else if let Some(v) = line.strip_prefix("Home-page:") {
            urls.push(("Homepage".to_string(), v.trim().to_string()));
        }
    }
    if let Ok(doc) = pyproject.parse::<toml::Table>()
        && let Some(t) = doc
            .get("project")
            .and_then(|p| p.get("urls"))
            .and_then(|u| u.as_table())
    {
        for (k, v) in t {
            if let Some(u) = v.as_str() {
                urls.push((k.clone(), u.trim().to_string()));
            }
        }
    }
    let rank = |k: &str| -> usize {
        let k = k.to_ascii_lowercase();
        [
            "source",
            "source code",
            "repository",
            "code",
            "github",
            "gitlab",
        ]
        .iter()
        .position(|s| *s == k)
        .unwrap_or(10)
    };
    urls.sort_by_key(|(k, _)| rank(k));
    urls.iter().find_map(|(_, u)| {
        let f = crate::forge::parse(u)?;
        let host_ok = f.host == "github.com"
            || f.host == "gitlab.com"
            || (f.host.starts_with("gitlab.") && crate::forge::is_known_host(&f.host));
        (host_ok && f.project_path.contains('/')).then(|| f.canonical_https())
    })
}

#[cfg(test)]
mod source_repo_tests {
    use super::source_repo_from_metadata as repo;

    #[test]
    fn source_keys_beat_homepage_and_other_hosts_are_skipped() {
        let pi = "Metadata-Version: 2.1\nProject-URL: Documentation, https://docs.example.org/x\n\
                  Project-URL: Homepage, https://github.com/o/home\n\
                  Project-URL: Source, git@github.com:o/real.git\n\nProject-URL: Source, https://github.com/o/body\n";
        assert_eq!(repo(pi, "").as_deref(), Some("https://github.com/o/real"));
        let py = "[project.urls]\nHomepage = \"https://gitlab.com/g/sub/proj/-/issues\"\n";
        assert_eq!(
            repo("", py).as_deref(),
            Some("https://gitlab.com/g/sub/proj")
        );
        assert_eq!(
            repo("Project-URL: Home, https://bitbucket.org/o/r\n", ""),
            None
        );
        assert_eq!(repo("Home-page: https://github.com/onlyowner\n", ""), None);
        assert_eq!(repo("", "not toml ["), None);
    }
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
    fn a_declared_name_that_is_not_a_plain_project_name_is_dropped() {
        for ok in ["Flask", "zope.interface", "a_b-c", "libsigc++"] {
            assert!(is_plain_project_name(ok), "{ok}");
        }
        for bad in ["", "../x", "a/b", "a b", "-x", "a;rm", "a<b", "a%{x}", "é"] {
            assert!(!is_plain_project_name(bad), "{bad}");
        }
    }

    #[test]
    fn pyproject_scripts_are_read_and_unsafe_names_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        write_tar(
            &tarball,
            &[(
                "thing-1/pyproject.toml",
                b"[project]\nname = \"thing\"\n[project.scripts]\nthing = \"thing:main\"\n\
                  \"../x\" = \"a:b\"\n[project.gui-scripts]\nthing-gui = \"thing:gui\"\n"
                    .as_slice(),
            )],
        );
        let info = inspect(&tarball).unwrap();
        assert_eq!(info.console_scripts, vec!["thing", "thing-gui"]);
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

    enum Member<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Link(&'a str, &'a str),
    }

    fn write_members(path: &std::path::Path, members: &[Member<'_>]) {
        let file = std::fs::File::create(path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        for m in members {
            let mut header = tar::Header::new_gnu();
            match m {
                Member::File(name, body) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(body.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    ar.append_data(&mut header, name, *body).unwrap();
                }
                Member::Dir(name) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_cksum();
                    ar.append_data(&mut header, name, std::io::empty()).unwrap();
                }
                Member::Link(name, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_mode(0o777);
                    ar.append_link(&mut header, name, target).unwrap();
                }
            }
        }
        ar.into_inner().unwrap().finish().unwrap();
    }

    /// The chain from the review: `x` is a link that resolves back to the
    /// root, so `x/y` is really `y`, and a link created under `x/y` climbs
    /// out of the archive. Each step passes the lexical check on its own.
    #[test]
    fn a_chain_of_links_cannot_redirect_a_later_link_outside() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"host secret").unwrap();
        let tarball = dir.path().join("pkg.tar.gz");
        write_members(
            &tarball,
            &[
                Member::Dir("a/"),
                Member::Dir("a/b/"),
                Member::Link("x", "a/b/../.."),
                Member::Dir("x/y/"),
                Member::Link("x/y/link", "../../outside"),
            ],
        );
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();

        let err = extract(&tarball, &root).unwrap_err();

        assert!(matches!(err, Error::LinkEscapes { .. }), "{err}");
        assert!(!root.join("y/link").exists() && !root.join("x/y/link").exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"host secret");
    }

    #[test]
    fn links_that_stay_inside_the_archive_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("pkg.tar.gz");
        write_members(
            &tarball,
            &[
                Member::Dir("pkg-1/"),
                Member::Dir("pkg-1/sub/"),
                Member::File("pkg-1/lib.so.1", b"elf"),
                Member::Link("pkg-1/lib.so", "lib.so.1"),
                Member::Link("pkg-1/sub/up", "../lib.so"),
                Member::Link("pkg-1/a/d", "../lib.so.1"),
            ],
        );
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();

        extract(&tarball, &root).unwrap();

        assert_eq!(std::fs::read(root.join("pkg-1/sub/up")).unwrap(), b"elf");
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

    /// NVIDIA's archives are `.tar.xz`: the member list comes from the stream,
    /// directories keep their trailing slash, and nothing is unpacked.
    #[test]
    fn list_archive_members_reads_a_tar_xz_without_extracting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz");
        let mut raw = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut raw);
            let mut d = tar::Header::new_gnu();
            d.set_entry_type(tar::EntryType::Directory);
            d.set_size(0);
            d.set_mode(0o755);
            d.set_cksum();
            ar.append_data(&mut d, "top/lib", std::io::empty()).unwrap();
            for name in ["top/LICENSE", "top/lib/libx.so.1"] {
                let mut h = tar::Header::new_gnu();
                h.set_size(2);
                h.set_mode(0o644);
                h.set_cksum();
                ar.append_data(&mut h, name, &b"hi"[..]).unwrap();
            }
            ar.finish().unwrap();
        }
        let mut w = lzma_rust2::XzWriter::new(
            std::fs::File::create(&path).unwrap(),
            lzma_rust2::XzOptions::default(),
        )
        .unwrap();
        std::io::Write::write_all(&mut w, &raw).unwrap();
        w.finish().unwrap();

        assert_eq!(
            list_archive_members(&path).unwrap(),
            ["top/lib/", "top/LICENSE", "top/lib/libx.so.1"]
        );
        // Only the archive is in the directory.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn list_archive_members_refuses_a_zip() {
        assert!(list_archive_members(Path::new("x.whl")).is_err());
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
    fn extract_refuses_a_hard_link_through_a_symlinked_directory() {
        let build = |hard_target: &str| {
            let mut buf = Vec::new();
            {
                let mut ar = tar::Builder::new(&mut buf);
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o777);
                ar.append_link(&mut h, "pkg/d", "sub").unwrap();
                let mut h = tar::Header::new_gnu();
                h.set_size(1);
                h.set_mode(0o644);
                h.set_cksum();
                ar.append_data(&mut h, "pkg/sub/f", b"x".as_slice())
                    .unwrap();
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Link);
                h.set_size(0);
                h.set_mode(0o644);
                ar.append_link(&mut h, "pkg/h", hard_target).unwrap();
                ar.finish().unwrap();
            }
            let dir = tempfile::tempdir().unwrap();
            let tgz = dir.path().join("a.tar.gz");
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut enc, &buf).unwrap();
            std::fs::write(&tgz, enc.finish().unwrap()).unwrap();
            let dest = dir.path().join("out");
            (extract(&tgz, &dest), dir, dest)
        };
        let (result, _dir, _) = build("pkg/d/f");
        assert!(
            matches!(result, Err(Error::LinkEscapes { .. })),
            "{result:?}"
        );
        let (result, _dir, dest) = build("pkg/sub/f");
        result.unwrap();
        assert!(dest.join("pkg/h").is_file());
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
    fn extract_where_writes_only_the_kept_entries() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.tar.gz");
        write_tar(
            &src,
            &[
                ("thing-1.0/README", b"hello\n"),
                ("thing-1.0/setup.py", b"print(1)\n"),
            ],
        );
        let out = dir.path().join("out");
        extract_where(&src, &out, |p| p.ends_with("setup.py")).unwrap();
        assert!(out.join("thing-1.0").join("setup.py").is_file());
        assert!(!out.join("thing-1.0").join("README").exists());
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

    #[test]
    fn failed_extract_leaves_neither_dest_nor_partial_behind() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("broken.tar.gz");
        std::fs::write(&src, b"not gzip at all").unwrap();
        let out = dir.path().join("sdist");
        assert!(extract(&src, &out).is_err());
        assert!(!out.exists());
        assert!(!dir.path().join("sdist.partial").exists());
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

    /// A top-level name that would be written unescaped into the spec must not
    /// be taken: newline, RPM macro, whitespace and shell metacharacters.
    #[test]
    fn unsafe_top_level_names_are_not_taken() {
        for bad in [
            "evil\n%post\nrm -rf /\n-1.0",
            "pct%{name}-1.0",
            "sp ace-1.0",
            "semi;rm-1.0",
            "dollar$(x)-1.0",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tarball = dir.path().join("src.tar.gz");
            let member = format!("{bad}/setup.py");
            write_tar(&tarball, &[(member.as_str(), b"x".as_slice())]);
            let info = inspect(&tarball).unwrap();
            assert_eq!(info.top_level_dir, None, "accepted {bad:?}");
        }
    }

    #[test]
    fn is_safe_top_level_dir_accepts_real_source_tree_names() {
        for good in [
            "thing-1.0",
            "MarkupSafe-3.0.3",
            "sqlite3-3",
            "a_b+c~1",
            "v1.2",
            "Pyöpt-1.0",
        ] {
            assert!(is_safe_top_level_dir(good), "{good}");
        }
        for bad in [
            "", ".", "..", "a/b", "a\\b", "a b", "a\nb", "a\u{7}b", "a%b", "a;b", "a$b", "a\"b",
            "a'b", "a`b", "a(b)", "a{b}", "a|b", "a&b", "a<b>", "a*b", "a?b", "a!b", "a#b",
        ] {
            assert!(!is_safe_top_level_dir(bad), "{bad:?}");
        }
    }

    /// Python's tarfile writes directory members without a trailing slash; they
    /// must be recognised as directories and kept out of the file listing.
    #[test]
    fn directory_entries_without_trailing_slash_are_not_files() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        {
            let file = std::fs::File::create(&tarball).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut ar = tar::Builder::new(enc);
            let mut d = tar::Header::new_gnu();
            d.set_entry_type(tar::EntryType::Directory);
            d.set_size(0);
            d.set_mode(0o755);
            d.set_cksum();
            ar.append_data(&mut d, "thing-1.0/lib", std::io::empty())
                .unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, "thing-1.0/lib/x.py", &b"hi"[..])
                .unwrap();
            ar.into_inner().unwrap().finish().unwrap();
        }
        let info = inspect(&tarball).unwrap();
        assert_eq!(info.files, vec!["thing-1.0/lib/x.py".to_string()]);
        assert_eq!(info.top_level_dir.as_deref(), Some("thing-1.0"));
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

    #[test]
    fn archive_budget_refuses_a_decompression_bomb() {
        // A 1 MiB archive may expand to 64 MiB + 200 MiB, not to gigabytes.
        let mut b = Budget {
            compressed: 1 << 20,
            ..Default::default()
        };
        assert!(b.charge(100 << 20).is_ok());
        assert!(matches!(b.charge(300 << 20), Err(Error::LimitExceeded(_))));
        // An unknown archive size turns the ratio check off.
        let mut unknown = Budget::default();
        assert!(unknown.charge(1 << 30).is_ok());
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
    fn wheel_layout_reads_the_files_dist_info_and_requires_dist() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("m.whl");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&w).unwrap());
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        zw.start_file("pkg/__init__.py", opts).unwrap();
        zw.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
        write!(
            zw,
            "Metadata-Version: 2.4\nName: pkg\nRequires-Dist: numpy>=1.20\nRequires-Dist: tomli; python_version < \"3.11\"\n\nRequires-Dist: not-a-header\n"
        )
        .unwrap();
        zw.finish().unwrap();
        let l = wheel_layout(&w).unwrap();
        assert_eq!(l.dist_info.as_deref(), Some("pkg-1.0.dist-info"));
        assert_eq!(l.files, ["pkg/__init__.py", "pkg-1.0.dist-info/METADATA"]);
        assert_eq!(
            l.requires_dist,
            ["numpy>=1.20", "tomli; python_version < \"3.11\""]
        );
    }

    #[test]
    fn wheel_layout_reads_the_first_prose_paragraph_of_the_description() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("d.whl");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&w).unwrap());
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        zw.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
        write!(
            zw,
            "Metadata-Version: 2.4\nName: pkg\nSummary: s\n\n# pkg\n\n[![CI](x)](y)\n\nPkg is a fast thing that does\nuseful work, 100% of the time.\n\nSecond paragraph.\n"
        )
        .unwrap();
        zw.finish().unwrap();
        let l = wheel_layout(&w).unwrap();
        assert_eq!(
            l.description.as_deref(),
            Some("Pkg is a fast thing that does useful work, 100% of the time.")
        );
        assert_eq!(metadata_description("Name: x\n\n# only a heading\n"), None);
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

    fn write_wheel_with(path: &Path, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(body).unwrap();
        }
        zw.finish().unwrap();
    }

    /// A minimal ELF64 little-endian shared object: header, a `PT_LOAD` over the
    /// whole file, a `PT_DYNAMIC`, a `.dynamic` with `DT_STRTAB`/`DT_STRSZ` and
    /// one `dyn_tag` entry (`DT_RUNPATH` = 29, `DT_RPATH` = 15) naming `value`
    /// (`None`: no such entry), and the string table. The string table is
    /// mapped at a virtual address unlike its file offset, as in real files.
    fn elf_with_dynamic(dyn_tag: u64, value: Option<&str>) -> Vec<u8> {
        const BASE: u64 = 0x40_0000;
        let strtab = format!("\0{}\0", value.unwrap_or(""));
        let (phoff, dynoff, stroff) = (64u64, 64 + 2 * 56, 64 + 2 * 56 + 64);
        let total = stroff + strtab.len() as u64;
        let mut out = vec![0u8; 64];
        out[..4].copy_from_slice(b"\x7fELF");
        out[4] = 2;
        out[5] = 1;
        out[32..40].copy_from_slice(&phoff.to_le_bytes());
        out[54..56].copy_from_slice(&56u16.to_le_bytes());
        out[56..58].copy_from_slice(&2u16.to_le_bytes());
        for (ty, off, vaddr, size) in [(1u32, 0, BASE, total), (2, dynoff, BASE + dynoff, 64)] {
            out.extend(ty.to_le_bytes());
            out.extend(4u32.to_le_bytes());
            for v in [off, vaddr, vaddr, size, size, 8] {
                out.extend(u64::to_le_bytes(v));
            }
        }
        let tag = if value.is_some() { dyn_tag } else { 1 };
        for (t, v) in [
            (5, BASE + stroff),
            (10, strtab.len() as u64),
            (tag, 1),
            (0, 0),
        ] {
            out.extend(u64::to_le_bytes(t));
            out.extend(u64::to_le_bytes(v));
        }
        out.extend(strtab.as_bytes());
        out
    }

    /// python-triton's shape: `proton`/`proton-viewer` are generated into
    /// `/usr/bin` from `entry_points.txt`, and only `[console_scripts]` (and
    /// `[gui_scripts]`) name files there — other groups are plugins.
    /// A wheel from before `License-Expression`: the free-text header and the
    /// trove classifiers are read, `UNKNOWN` and a body line are not a licence.
    #[test]
    fn wheel_layout_reads_the_legacy_licence_headers() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.whl");
        write_wheel_with(
            &old,
            &[
                ("pkg/__init__.py", b""),
                (
                    "pkg-1.0.dist-info/METADATA",
                    b"Metadata-Version: 2.1\nName: pkg\nLicense: 3-Clause BSD\n\
                      Classifier: License :: OSI Approved :: BSD License\n\
                      Classifier: Programming Language :: Python\n\nLicense: not a header\n",
                ),
            ],
        );
        let l = wheel_layout(&old).unwrap();
        assert_eq!(l.license_text.as_deref(), Some("3-Clause BSD"));
        assert_eq!(
            l.license_classifiers,
            vec!["License :: OSI Approved :: BSD License"]
        );
        assert_eq!(l.license_expression, None);

        let new = dir.path().join("new.whl");
        write_wheel_with(
            &new,
            &[
                ("pkg/__init__.py", b""),
                (
                    "pkg-1.0.dist-info/METADATA",
                    b"Name: pkg\nLicense: UNKNOWN\nLicense-Expression: MIT\n\n",
                ),
            ],
        );
        let l = wheel_layout(&new).unwrap();
        assert_eq!(l.license_expression.as_deref(), Some("MIT"));
        assert_eq!(l.license_text, None);
    }

    #[test]
    fn wheel_layout_reads_console_scripts_from_entry_points() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("e.whl");
        write_wheel_with(
            &w,
            &[
                ("pkg/__init__.py", b""),
                (
                    "pkg-1.0.dist-info/entry_points.txt",
                    b"# generated\n[console_scripts]\nproton = proton.profile:main\n\
                      proton-viewer=proton.viewer:main [viewer]\nproton = dup:main\n\
                      ; skipped\n\n[gui_scripts]\npkg-gui = pkg.gui:run\n\
                      [pkg.plugins]\nnot-a-script = pkg.plug:Plugin\n",
                ),
            ],
        );
        let l = wheel_layout(&w).unwrap();
        assert_eq!(
            l.console_scripts,
            vec!["proton", "proton-viewer", "pkg-gui"]
        );

        let n = dir.path().join("n.whl");
        write_wheel(&n, &["pkg/__init__.py", "pkg-1.0.dist-info/RECORD"]);
        assert!(wheel_layout(&n).unwrap().console_scripts.is_empty());
    }

    /// Script names go into a generated spec, so anything that is not a plain
    /// file name (option-looking, a path, a macro, whitespace) is dropped.
    #[test]
    fn console_scripts_drop_names_that_are_not_plain_file_names() {
        let text = "[console_scripts]\n--evil = a:b\n../x = a:b\n%{foo} = a:b\n\
                    a b = a:b\n.hidden = a:b\nok.name_1+2-x = a:b\nno-equals\n";
        assert_eq!(
            console_scripts_from_entry_points(text),
            vec!["ok.name_1+2-x"]
        );
    }

    /// opencv-python-headless' shape: auditwheel's `<name>.libs/` directory of
    /// hash-suffixed libraries, and hash-suffixed names found elsewhere.
    #[test]
    fn wheel_layout_detects_auditwheel_hashed_libs() {
        let dir = tempfile::tempdir().unwrap();
        let in_libs = dir.path().join("a.whl");
        write_wheel(
            &in_libs,
            &[
                "cv2/__init__.py",
                "opencv_python_headless.libs/libavcodec-f34387fd.so.62.11.100",
            ],
        );
        assert!(wheel_layout(&in_libs).unwrap().has_hashed_libs);
        let inline = dir.path().join("b.whl");
        write_wheel(&inline, &["numpy/_core/libopenblasp-r0-37b5f859.3.3.so"]);
        assert!(wheel_layout(&inline).unwrap().has_hashed_libs);
        // Ordinary sonames, a date stamp, and a hash outside a library are not.
        let plain = dir.path().join("c.whl");
        write_wheel(
            &plain,
            &[
                "p/libcuda.so.1",
                "p/libz3.so.4.15",
                "p/libfoo-1.2.so",
                "p/libbar-20240101.so.1",
                "p/data-f34387fd.json",
                "pkg-1.0.dist-info/libx-f34387fd.so.1",
            ],
        );
        assert!(!wheel_layout(&plain).unwrap().has_hashed_libs);
        // The names-only check agrees, and reads no wheel that is not one.
        assert!(wheel_has_hashed_libs(&in_libs));
        assert!(wheel_has_hashed_libs(&inline));
        assert!(!wheel_has_hashed_libs(&plain));
        assert!(!wheel_has_hashed_libs(&dir.path().join("absent.whl")));
    }

    /// cuda-bindings, nccl4py: no `cuda/__init__.py`, a regular package below it.
    #[test]
    fn a_namespace_wheel_offers_its_shallowest_regular_package() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("a.whl");
        write_wheel(
            &ns,
            &[
                "cuda/bindings/__init__.py",
                "cuda/bindings/_internal/__init__.py",
                "cuda/bindings/tests/__init__.py",
                "cuda_bindings-1.dist-info/METADATA",
            ],
        );
        let l = wheel_layout(&ns).unwrap();
        assert_eq!(l.importable, None);
        assert_eq!(l.namespace_module.as_deref(), Some("cuda.bindings"));
        // A regular top-level package is the importable name, no namespace needed.
        let reg = dir.path().join("b.whl");
        write_wheel(&reg, &["pkg/__init__.py", "pkg/sub/__init__.py"]);
        let l = wheel_layout(&reg).unwrap();
        assert_eq!(l.importable.as_deref(), Some("pkg"));
        assert_eq!(l.namespace_module, None);
        // Only libraries: nothing to find.
        let libs = dir.path().join("c.whl");
        write_wheel(&libs, &["nvidia/cu13/lib/libx.so.1"]);
        assert_eq!(wheel_layout(&libs).unwrap().namespace_module, None);
    }

    /// libcusparseLt.so.0 (RUNPATH: $ORIGIN:): found in a `DT_RUNPATH` or a
    /// `DT_RPATH`, not for another value, no entry, or a non-ELF file.
    /// open-webui's wheel: empty `custom.css`/`loader.js` are listed (rpmlint's
    /// `zero-length`), an empty `__init__.py` and dist-info files are not.
    #[test]
    fn empty_files_are_listed_except_those_rpmlint_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("e.whl");
        write_wheel_with(
            &w,
            &[
                ("pkg/__init__.py", b""),
                ("pkg/static/loader.js", b""),
                ("pkg/static/custom.css", b""),
                ("pkg/static/full.css", b"a{}"),
                ("pkg/py.typed", b""),
                ("pkg-1.dist-info/EMPTY", b""),
            ],
        );
        assert_eq!(
            wheel_layout(&w).unwrap().empty_files,
            ["pkg/static/custom.css", "pkg/static/loader.js"]
        );
    }

    #[test]
    fn origin_runpath_is_read_from_the_dynamic_section() {
        let found = |bytes: &[u8]| elf_has_origin_runpath(std::io::Cursor::new(bytes));
        assert!(found(&elf_with_dynamic(29, Some("$ORIGIN:"))));
        assert!(found(&elf_with_dynamic(15, Some("/opt/x:$ORIGIN/../lib"))));
        assert!(!found(&elf_with_dynamic(29, Some("/opt/lib"))));
        assert!(!found(&elf_with_dynamic(29, None)));

        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("o.whl");
        write_wheel_with(
            &w,
            &[
                ("nvidia/lib/libnotelf.so.1", &[b'x'; 200]),
                (
                    "nvidia/lib/libcusparseLt.so.0",
                    &elf_with_dynamic(29, Some("$ORIGIN:")),
                ),
            ],
        );
        assert!(wheel_layout(&w).unwrap().origin_runpath);
        let w = dir.path().join("p.whl");
        write_wheel_with(
            &w,
            &[(
                "nvidia/lib/libfoo.so.1",
                &elf_with_dynamic(29, Some("/opt")),
            )],
        );
        assert!(!wheel_layout(&w).unwrap().origin_runpath);
        // A library under .dist-info is not part of the payload.
        let w = dir.path().join("q.whl");
        write_wheel_with(
            &w,
            &[(
                "p-1.dist-info/libx.so",
                &elf_with_dynamic(29, Some("$ORIGIN")),
            )],
        );
        assert!(!wheel_layout(&w).unwrap().origin_runpath);
    }

    /// Malformed input is `false`, never a panic: every prefix of a valid file,
    /// a flipped byte at each position, 32-bit and big-endian classes, absurd
    /// offsets and sizes.
    #[test]
    fn origin_runpath_survives_malformed_elf_files() {
        let good = elf_with_dynamic(29, Some("$ORIGIN:"));
        let found = |bytes: &[u8]| elf_has_origin_runpath(std::io::Cursor::new(bytes));
        assert!(found(&good));
        // The string table starts at 240; cut inside `$ORIGIN` is still a hit.
        for len in 0..good.len() {
            assert_eq!(found(&good[..len]), len >= 248, "truncated to {len}");
        }
        for at in 0..good.len() {
            for flip in [0x00u8, 0xff] {
                let mut bad = good.clone();
                bad[at] = flip;
                let _ = found(&bad);
            }
        }
        let mut bad = good.clone();
        bad[4] = 1; // ELFCLASS32
        assert!(!found(&bad));
        let mut bad = good.clone();
        bad[5] = 2; // big-endian
        assert!(!found(&bad));
        let mut bad = good.clone();
        bad[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // e_phoff
        assert!(!found(&bad));
        let mut bad = good.clone();
        bad[56..58].copy_from_slice(&u16::MAX.to_le_bytes()); // e_phnum
        assert!(!found(&bad));
        // PT_DYNAMIC at an offset far past the end of the file.
        let mut bad = good.clone();
        bad[64 + 56 + 8..64 + 56 + 16].copy_from_slice(&(u64::MAX - 8).to_le_bytes());
        assert!(!found(&bad));
        assert!(!found(b""));
        assert!(!found(&[0x7f; 4096]));
    }

    #[test]
    fn wheel_layout_accepts_a_single_module_wheel() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path().join("m.whl");
        write_wheel(&w, &["six.py", "six-1.0.dist-info/METADATA"]);
        assert_eq!(wheel_layout(&w).unwrap().importable.as_deref(), Some("six"));
    }

    /// Live resolution of an sdist's source repository from the Project-URL
    /// entries PyPI serves for real projects: public GETs only. Run with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore = "network: GETs pypi.org"]
    fn live_source_repo_from_the_metadata_pypi_serves() {
        for (project, want) in [
            ("flask", "https://github.com/pallets/flask"),
            ("requests", "https://github.com/psf/requests"),
            ("numpy", "https://github.com/numpy/numpy"),
        ] {
            let url = format!("https://pypi.org/pypi/{project}/json");
            let body = crate::net::call_with_retries(|| {
                crate::net::agent(ureq::Agent::config_builder()).get(&url)
            })
            .unwrap()
            .into_body()
            .read_to_string()
            .unwrap();
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            let info = &json["info"];
            let mut pkg_info = String::from("Metadata-Version: 2.1\n");
            if let Some(urls) = info["project_urls"].as_object() {
                for (k, v) in urls {
                    pkg_info.push_str(&format!("Project-URL: {k}, {}\n", v.as_str().unwrap_or("")));
                }
            }
            if let Some(h) = info["home_page"].as_str().filter(|h| !h.is_empty()) {
                pkg_info.push_str(&format!("Home-page: {h}\n"));
            }
            assert_eq!(
                source_repo_from_metadata(&pkg_info, "").as_deref(),
                Some(want),
                "{project}: {pkg_info}"
            );
        }
    }
}
