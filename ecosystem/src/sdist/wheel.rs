use super::*;

/// Read up to [`MAX_MANIFEST_BYTES`] of `entry` as lossy UTF-8. `None` when
/// the read fails — an unreadable nested manifest must not fail inspection.
pub(super) fn read_manifest(entry: &mut impl Read) -> Option<String> {
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
pub(super) fn console_scripts_from_entry_points(text: &str) -> Vec<String> {
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

/// The shallowest regular package of a wheel that has none at its top level: the
/// directory with an `__init__.py` closest to the root, as a dotted name
/// (`cuda/bindings/__init__.py` is `cuda.bindings`). Every component must be a
/// plain identifier, and test directories are not offered.
fn namespace_module_of(names: &[String], skip: &dyn Fn(&str) -> bool) -> Option<String> {
    let is_ident = is_python_identifier;
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
        .find(|top| top.ends_with(".dist-info") && is_shell_safe_member(top))
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
        if is_pkg && is_python_identifier(&top) {
            importable = Some(top);
            break;
        }
        if is_mod && importable.is_none() {
            let module = n.trim_end_matches(".py");
            if is_python_identifier(module) {
                importable = Some(module.to_string());
            }
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
            !skip(&top_of(n))
                && (file.ends_with(".so") || file.contains(".so."))
                && is_shell_safe_member(n)
        })
        .min_by_key(|n| n.len())
        .cloned();
    let header_dirs: Vec<String> = {
        let mut dirs: Vec<String> = names
            .iter()
            .filter(|n| {
                let file = n.rsplit('/').next().unwrap_or("");
                !skip(&top_of(n))
                    && (file.ends_with(".h") || file.ends_with(".hpp"))
                    && is_shell_safe_member(n)
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
        // subdirectory too would just be redundant noise. Sorting puts a
        // parent before its descendants (a string prefix sorts first), but
        // not right after them: `a/b`, `a/b-x`, `a/b/c` puts `a/b-x` in
        // between. So each directory is checked against every kept one.
        let mut kept: Vec<String> = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let is_nested_under_kept = kept.iter().any(|parent: &String| {
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
pub(super) fn metadata_description(metadata: &str) -> Option<String> {
    // CRLF files have no bare "\n\n": normalise first so the header/body split
    // happens at the first blank line whichever line ending it uses.
    let normalised = metadata.replace("\r\n", "\n");
    let body = normalised
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
