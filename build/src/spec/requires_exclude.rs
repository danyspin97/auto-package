//! The requires-exclude rules of a wheel spec: the hashed libraries, the driver
//! libraries, and the alternatives they add.

/// The `__requires_exclude` regex for a wheel bundling auditwheel-style
/// libraries (`libavcodec-f34387fd.so.62.11.100`): the generated `Requires`
/// name those hashed sonames, and nothing can provide them. Requires 8 hex
/// digits after a `-` in a `lib*` name, so `libcuda.so.1`, `libz3.so.4.15`
/// and `libfoo-1.2.so` are left alone, nor a date-stamped `libfoo-20240101.so`.
/// Generated from the same rule as `ap_ecosystem::sdist::is_hashed_soname`
/// (the one definition of "hashed"); convention 68 of the agent prompt and the
/// reason text of `depclass.rs` describe it in words.
pub(super) fn hashed_libs_requires_exclude() -> &'static str {
    ap_ecosystem::sdist::hashed_soname_requires_exclude()
}

/// The regex an earlier generator wrote: it also matched date-stamped names.
/// Specs carrying it are rewritten to the current one.
pub(super) const OLD_HASHED_LIBS_REQUIRES_EXCLUDE: &str = r"^lib.*-[0-9a-f]{8}.*\.so.*$";

/// The `%global __requires_exclude` line for a wheel that bundles hashed
/// libraries, empty otherwise. rpm honours only ONE such global: the patterns
/// to exclude are folded into a single regex with `|`, which is what a second
/// pattern added to this skeleton has to do.
pub(super) fn wheel_requires_exclude_line(
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    is_pure: bool,
) -> String {
    let mut patterns: Vec<&str> = Vec::new();
    if layout.is_some_and(|l| l.has_hashed_libs) {
        patterns.push(hashed_libs_requires_exclude());
    }
    // A compiled wheel may link the NVIDIA driver (`libcuda.so.1`), which no
    // repository provides: its Requires never name it.
    if !is_pure {
        patterns.push(crate::depclass::driver_requires_exclude());
    }
    let mut line = String::new();
    for p in patterns {
        line = if line.is_empty() {
            format!("%global __requires_exclude {p}\n")
        } else {
            with_requires_exclude_pattern(&format!("{line}%description\n"), p)
                .trim_end_matches("%description\n")
                .to_string()
        };
    }
    line
}

/// `spec` with the hashed-library `__requires_exclude` of a wheel that bundles
/// such libraries (see [`hashed_libs_requires_exclude`]), for a spec written
/// before the generator knew to: python-opencv-python-headless was built, was
/// uninstallable, and every package needing it stayed unresolvable. An existing
/// `%global __requires_exclude` is folded into one regex with `|` (rpm honours
/// only one); the line is otherwise added before `%python_subpackages`, or
/// `%description`. A spec that already carries the pattern is returned as is.
pub fn with_hashed_libs_requires_exclude(spec: &str) -> String {
    let spec = &spec.replace(
        OLD_HASHED_LIBS_REQUIRES_EXCLUDE,
        hashed_libs_requires_exclude(),
    );
    with_requires_exclude_pattern(spec, hashed_libs_requires_exclude())
}

/// `spec` that leaves the NVIDIA driver's libraries (`libcuda.so.1`, NVML, ...:
/// [`crate::depclass::is_driver_soname`]) out of its Requires: nothing provides
/// them, the host driver does at run time, so a package that links one is
/// unresolvable in OBS (python-flashinfer-python: `nothing provides
/// libcuda.so.1()(64bit)`). An existing `%global __requires_exclude` is
/// extended, never overwritten.
pub fn with_driver_requires_exclude(spec: &str) -> String {
    with_requires_exclude_pattern(spec, crate::depclass::driver_requires_exclude())
}

/// The inside of an anchored `__requires_exclude` regex, without `^`/`$`.
pub(super) fn requires_exclude_inner(pattern: &str) -> String {
    pattern
        .trim()
        .trim_start_matches('^')
        .trim_end_matches('$')
        .to_string()
}

/// `spec` with `pattern` (an anchored regex) part of its one
/// `%global __requires_exclude`: folded into an existing line with `|` (rpm
/// honours only one such global), or added before `%python_subpackages` /
/// `%description`. A spec already carrying the pattern is returned as is.
fn with_requires_exclude_pattern(spec: &str, pattern: &str) -> String {
    let wanted = requires_exclude_inner(pattern);
    let lines: Vec<&str> = spec.lines().collect();
    let tail = if spec.ends_with('\n') { "\n" } else { "" };
    if let Some(at) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("%global __requires_exclude "))
    {
        let existing = lines[at]
            .trim_start()
            .trim_start_matches("%global __requires_exclude ");
        if existing.contains(&wanted) {
            return spec.to_string();
        }
        let folded = format!(
            "%global __requires_exclude ^({}|{})$",
            requires_exclude_inner(existing),
            wanted
        );
        let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        out[at] = folded;
        return out.join("\n") + tail;
    }
    let Some(at) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("%python_subpackages"))
        .or_else(|| {
            lines
                .iter()
                .position(|l| l.trim_start().starts_with("%description"))
        })
    else {
        return spec.to_string();
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at, format!("%global __requires_exclude {pattern}"));
    out.join("\n") + tail
}

/// Whether a spec builds an architecture-specific package that may link the
/// driver's library: not `BuildArch: noarch`, and it names CUDA or NVIDIA.
pub fn may_link_driver_library(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    let noarch = lower.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("buildarch:") && l.contains("noarch") && !l.contains('%')
    });
    !noarch && (lower.contains("cuda") || lower.contains("nvidia"))
}

/// Spec fragments that give a wheel's console scripts the openSUSE
/// multi-flavor `update-alternatives` handling a source build gets (convention
/// 18): `(requires, clone, scriptlets, files)`, each empty when the wheel
/// declares no script. pip generates `/usr/bin/<script>` per flavor, so it
/// must be cloned to `<script>-<pyver>` and owned through
/// `%python_alternative`, or the build fails with `Installed (but unpackaged)
/// file(s) found` (python-triton's `proton`, python-cuda-tile's
/// `cutile-cache`).
pub(super) fn wheel_alternatives_fragments(
    scripts: &[String],
) -> (&'static str, String, String, String) {
    if scripts.is_empty() {
        return ("", String::new(), String::new(), String::new());
    }
    let each =
        |line: &dyn Fn(&str) -> String| -> String { scripts.iter().map(|s| line(s)).collect() };
    (
        "Requires(post):   update-alternatives\nRequires(postun): update-alternatives\n",
        each(&|s| format!("%python_clone -a %{{buildroot}}%{{_bindir}}/{s}\n")),
        format!(
            "%post\n{}\n%postun\n{}\n",
            each(&|s| format!("%python_install_alternative {s}\n")),
            each(&|s| format!("%python_uninstall_alternative {s}\n")),
        ),
        each(&|s| format!("%python_alternative %{{_bindir}}/{s}\n")),
    )
}

/// The Python (for `python -c`) that proves the dotted package `module` is
/// found under `root` alone: `PathFinder.find_spec` on each prefix, searching
/// only the locations the previous prefix found.
pub(super) fn namespace_find_spec_snippet(module: &str, root: &str) -> String {
    let mut parts = module.split('.');
    let top = parts.next().unwrap_or_default();
    let mut snippet = format!(
        "import importlib.machinery as m, sys; s = m.PathFinder.find_spec('{top}', ['{root}'])"
    );
    let mut prefix = top.to_string();
    for part in parts {
        prefix = format!("{prefix}.{part}");
        snippet.push_str(&format!(
            "; s = s and m.PathFinder.find_spec('{prefix}', s.submodule_search_locations)"
        ));
    }
    snippet.push_str("; sys.exit(0 if s else 1)");
    snippet
}
