use super::*;

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
            // A manifest at the archive root (a single component) has no
            // directory above it: it gets the empty directory, as a
            // two-component entry does, instead of an out-of-range slice.
            let dir = components
                .get(1..components.len().saturating_sub(1))
                .unwrap_or_default()
                .iter()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            // The directory ends up in `cd` and in a monorepo's spec comments:
            // a manifest under an unsafe component is not recorded at all.
            if is_safe_manifest_dir(&dir) {
                py_manifests.push((depth, dir, src.clone()));
            }
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

    let bin_names = match (&cargo, &top_level_dir) {
        (Some(((0, _), src)), Some(top)) => toml::from_str::<CargoToml>(src)
            .map(|d| d.bin_names(&files, top))
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let python_depth = match (pyproject.as_ref().map(|(d, _, _)| *d), setup_py_depth) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    // Name the subdirectory the packaging lives in. A chosen pyproject.toml
    // decides it when it is a packaging one (a `[build-system]` or `[project]`
    // table): its directory is the one the build reads the metadata from. A
    // tool-only pyproject.toml (pytest config in `tests/`) does not count: only
    // without a packaging pyproject is a setup.py/setup.cfg/Cargo.toml at the
    // depth used, and a pyproject.toml only where it is a packaging one.
    let is_packaging = |src: &str| {
        src.parse::<toml::Table>()
            .is_ok_and(|t| t.contains_key("build-system") || t.contains_key("project"))
    };
    let packaging_dirs: Vec<&str> = py_manifests
        .iter()
        .filter(|(_, _, src)| is_packaging(src))
        .map(|(_, dir, _)| dir.as_str())
        .collect();
    let packaging_depth = py_manifests
        .iter()
        .filter(|(_, _, src)| is_packaging(src))
        .map(|(d, _, _)| *d)
        .min();
    let fallback_depth = [
        packaging_depth,
        setup_py_depth,
        cargo.as_ref().map(|((d, _), _)| *d),
    ]
    .into_iter()
    .flatten()
    .min();
    let project_dir = match &pyproject {
        Some((depth, dir, src)) if is_packaging(src) => (*depth > 0).then(|| dir.clone()),
        _ => match fallback_depth {
            None | Some(0) => None,
            Some(depth) => {
                let dirs: Vec<String> = files
                    .iter()
                    .filter_map(|f| {
                        let rest = f.split_once('/').map(|(_, r)| r)?;
                        let parts: Vec<&str> = rest.split('/').collect();
                        if parts.len() != depth + 1 {
                            return None;
                        }
                        let dir = parts[..depth].join("/");
                        let ok = match *parts.last()? {
                            "pyproject.toml" => packaging_dirs.contains(&dir.as_str()),
                            "setup.py" | "setup.cfg" | "Cargo.toml" => true,
                            _ => false,
                        };
                        ok.then_some(dir)
                    })
                    .collect();
                choose_fallback_dir(dirs, normalized)
            }
        },
    };

    let native = native::find(&files, top_level_dir.as_deref());

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
        native,
        cmake_manifest: cmake_manifest && python_depth.is_none() && cargo.is_none(),
        autotools_manifest: autotools_manifest && python_depth.is_none() && cargo.is_none(),
        subprojects,
        files,
        console_scripts,
        bin_names,
    })
}
