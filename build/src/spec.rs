//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::process::{Command, Stdio};

use ap_ecosystem::pypi::{License, Release};
use ap_ecosystem::sdist::BuildSystem;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run py2pack: {0}")]
    Launch(std::io::Error),

    #[error("py2pack exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
mod fixes;
pub use fixes::*;
mod parse;
pub use parse::*;
mod sources;
pub use sources::*;
mod expand;
pub use expand::*;

mod generate;
#[cfg(test)]
mod tests;
pub use generate::*;
mod skeleton;
pub use skeleton::*;

/// Build a skeleton spec for a release that ships no sdist AND has no
/// discoverable source repository — a closed-source binary redistribution.
/// This is the normal shape of NVIDIA's individual CUDA toolkit component
/// wheels (`nvidia-cuda-nvcc`, `nvidia-cuda-cupti`, `nvidia-cuda-nvrtc`,
/// `nvidia-cuda-runtime`, ...): PyPI is simply how NVIDIA redistributes the
/// proprietary toolkit binaries, and there is nothing to compile.
///
/// `wheel_filename` becomes `Source0`; `%install` unpacks it directly with
/// `%pyproject_install`, which accepts a pre-built wheel path instead of a
/// dist it just built. See convention 24 Exception 3 for when this is the
/// right call versus a workaround to avoid.
/// Whether a repackaged wheel is the shape that reliably trips the same
/// handful of rpmlint false positives on an unmodified vendor binary: an
/// arch-specific wheel (not `-none-any.whl`) whose payload is a shared
/// library rather than importable Python. rpmlint judges these as if they
/// were a from-source build — a detached hash section, an executable bit on
/// the `.so`, `ldconfig` scriptlets, a runtime dependency on `python-base` —
/// none of which apply to a vendor `.so` dropped unmodified under
/// site-packages. See [`prebuilt_wheel_rpmlintrc`].
pub(crate) fn is_prebuilt_binary_wheel(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    let is_pure = wheel_filename.ends_with("-none-any.whl");
    !is_pure && layout.is_some_and(|l| l.first_shared_lib.is_some())
}

/// The `rpmlintrc` of a prebuilt/vendor-binary wheel (see
/// [`is_prebuilt_binary_wheel`]): only the findings that cannot be fixed in the
/// package, each with the reason it stays. What can be fixed is fixed in the
/// spec instead, by [`wheel_binary_skeleton`]: the executable bit on the
/// libraries, the dependency on `python-base`, the bundled headers.
///
/// - `missing-hash-section`: the libraries are upstream's prebuilt binaries,
///   linked without a build-id/hash section; they cannot be relinked here.
/// - `library-without-ldconfig-postin/postun`: the bundled libraries sit in
///   site-packages and are loaded by the Python modules from their own
///   directory, not from the ldconfig path, so there is no cache to refresh.
/// - `binary-or-shlib-defines-rpath` for `$ORIGIN`: vendor libraries find
///   their siblings that way, and the rule is badness 10000, so one hit aborts
///   rpmlint (`libcusparseLt.so.0 (RUNPATH: $ORIGIN:)`). Only written when a
///   bundled library really has such a RUNPATH
///   ([`ap_ecosystem::sdist::WheelLayout::origin_runpath`]): a filter nothing
///   matches is `unused-rpmlintrc-filter`, an error of its own (convention
///   72). Any other RPATH value still fails the check.
pub fn prebuilt_wheel_rpmlintrc(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::from(
        "# The libraries are upstream's prebuilt binaries, linked without a hash\n\
         # section; they cannot be relinked here.\n\
         addFilter(\"missing-hash-section\")\n\
         \n\
         # The bundled libraries live in site-packages and are loaded from their own\n\
         # directory by the Python modules, not from the ldconfig path: there is no\n\
         # ldconfig cache for a scriptlet to refresh.\n\
         addFilter(\"library-without-ldconfig-postin\")\n\
         addFilter(\"library-without-ldconfig-postun\")\n",
    );
    if layout.origin_runpath {
        out.push_str(
            "\n\
             # The bundled libraries find each other through an $ORIGIN RUNPATH, which\n\
             # is how a self-contained vendor wheel has to be laid out.\n\
             addFilter(\".*binary-or-shlib-defines-rpath.*[$]ORIGIN\")\n",
        );
    }
    out
}

/// The `zero-length` filters for the empty files a wheel ships (see
/// [`ap_ecosystem::sdist::WheelLayout::empty_files`]), one per file with the
/// reason. The files are upstream's own placeholders (`custom.css`,
/// `loader.js`): making them non-empty would change the payload, so rpmlint is
/// told instead. A name with a character outside the plain set is left out
/// rather than written into a regex unescaped.
fn wheel_empty_files_filters(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut out = String::new();
    for name in &layout.empty_files {
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-@+~".contains(c))
        {
            continue;
        }
        if out.is_empty() {
            out.push_str(
                "# Empty placeholder files that upstream's wheel ships on purpose (a\n\
                 # user-editable stylesheet or script): they must stay as they are.\n",
            );
        }
        let escaped = name.replace('.', "\\.").replace('+', "\\+");
        out.push_str(&format!(
            "addFilter(r\".*zero-length .*/site-packages/{escaped}$\")\n"
        ));
    }
    out
}

/// Whether a repackaged wheel needs a `python-<name>.rpmlintrc`: it is a
/// vendor-binary wheel, or it ships empty files rpmlint calls `zero-length`.
pub(crate) fn wheel_needs_rpmlintrc(
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> bool {
    is_prebuilt_binary_wheel(wheel_filename, layout)
        || layout.is_some_and(|l| !wheel_empty_files_filters(l).is_empty())
}

/// Write `python-<name>.rpmlintrc` next to the spec for a wheel that needs one
/// (the spec's `Source99:` names it, see [`wheel_needs_rpmlintrc`]); nothing for
/// any other wheel.
pub(crate) fn wheel_rpmlintrc(
    pkg_name: &str,
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> Option<(String, String)> {
    if !wheel_needs_rpmlintrc(wheel_filename, layout) {
        return None;
    }
    let layout = layout?;
    let mut text = String::new();
    if is_prebuilt_binary_wheel(wheel_filename, Some(layout)) {
        text.push_str(&prebuilt_wheel_rpmlintrc(layout));
    }
    let empties = wheel_empty_files_filters(layout);
    if !empties.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&empties);
    }
    // A zero-byte rpmlintrc is a `Source99` that does nothing: never written.
    if text.trim().is_empty() {
        return None;
    }
    Some((
        format!("python-{}.rpmlintrc", pkg_name.to_lowercase()),
        text,
    ))
}

/// The `__requires_exclude` regex for a wheel bundling auditwheel-style
/// libraries (`libavcodec-f34387fd.so.62.11.100`): the generated `Requires`
/// name those hashed sonames, and nothing can provide them. Requires 8 hex
/// digits after a `-` in a `lib*` name, so `libcuda.so.1`, `libz3.so.4.15`
/// and `libfoo-1.2.so` are left alone, nor a date-stamped `libfoo-20240101.so`.
/// Generated from the same rule as `ap_ecosystem::sdist::is_hashed_soname`
/// (the one definition of "hashed"); convention 68 of the agent prompt and the
/// reason text of `depclass.rs` describe it in words.
fn hashed_libs_requires_exclude() -> &'static str {
    ap_ecosystem::sdist::hashed_soname_requires_exclude()
}

/// The regex an earlier generator wrote: it also matched date-stamped names.
/// Specs carrying it are rewritten to the current one.
const OLD_HASHED_LIBS_REQUIRES_EXCLUDE: &str = r"^lib.*-[0-9a-f]{8}.*\.so.*$";

/// The `%global __requires_exclude` line for a wheel that bundles hashed
/// libraries, empty otherwise. rpm honours only ONE such global: the patterns
/// to exclude are folded into a single regex with `|`, which is what a second
/// pattern added to this skeleton has to do.
fn wheel_requires_exclude_line(
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
fn requires_exclude_inner(pattern: &str) -> String {
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
fn wheel_alternatives_fragments(scripts: &[String]) -> (&'static str, String, String, String) {
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
fn namespace_find_spec_snippet(module: &str, root: &str) -> String {
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

/// How `%install` clears one bundled header directory of a prebuilt wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderCleanup<'a> {
    /// The directory holds nothing but headers: `rm -rf` it.
    Dir(&'a str),
    /// The directory also holds files the package needs (a Python package's
    /// `__init__.py`, Cython sources, the `.c` a tool reads at run time): only
    /// its `.h`/`.hpp` files go.
    Headers(&'a str),
}

impl HeaderCleanup<'_> {
    /// Whether `%install` removes `path` (relative to site-packages).
    fn removes(&self, path: &str) -> bool {
        match self {
            HeaderCleanup::Dir(d) => path == *d || path.starts_with(&format!("{d}/")),
            HeaderCleanup::Headers(d) => {
                path.starts_with(&format!("{d}/"))
                    && (path.ends_with(".h") || path.ends_with(".hpp"))
            }
        }
    }
}

/// What `%install` removes of a wheel's bundled headers (see
/// [`HeaderCleanup`]); nothing for a wheel whose payload IS headers: when
/// everything outside the header directories is data (no `.py`, no shared
/// library, nothing under `bin/`), the wheel is a header library (NVIDIA's
/// `nvidia-cuda-crt` is only `include/crt/*.h`, `nvidia-cuda-cccl` is CCCL's
/// header tree), and stripping the headers leaves an empty package. Those are
/// split into a `-devel` package by the review (convention 24).
fn wheel_header_cleanups(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<HeaderCleanup<'_>> {
    // A hand-built layout can carry any directory name; one that is not shell
    // and RPM safe (`%{lua:...}`, `;`, `$(...)`, spaces) is never written into
    // `%install`, so it is left out of the cleanup and its headers stay.
    let header_dirs: Vec<&String> = layout
        .header_dirs
        .iter()
        .filter(|d| ap_ecosystem::sdist::is_shell_safe_member(d))
        .collect();
    let under = |d: &str, f: &str| f.starts_with(&format!("{d}/"));
    let is_payload = |f: &str| {
        let top = f.split('/').next().unwrap_or("");
        !top.ends_with(".dist-info") && !top.ends_with(".data") && top != "__pycache__"
    };
    let is_code = |f: &str| {
        let file = f.rsplit('/').next().unwrap_or("");
        file.ends_with(".py")
            || file.ends_with(".so")
            || file.contains(".so.")
            || f.split('/').rev().skip(1).any(|c| c == "bin")
    };
    let outside = |f: &String| is_payload(f) && !header_dirs.iter().any(|d| under(d, f));
    if header_dirs.is_empty()
        || !layout
            .files
            .iter()
            .filter(|f| outside(f))
            .any(|f| is_code(f))
    {
        return Vec::new();
    }
    let header_like = |f: &str| {
        let file = f.rsplit('/').next().unwrap_or(f);
        match file.rsplit_once('.') {
            None => true,
            Some((_, ext)) => matches!(ext, "h" | "hpp" | "hh" | "hxx" | "cuh" | "inl" | "ipp"),
        }
    };
    header_dirs
        .into_iter()
        .map(|d| {
            if layout
                .files
                .iter()
                .filter(|f| under(d, f))
                .all(|f| header_like(f))
            {
                HeaderCleanup::Dir(d.as_str())
            } else {
                HeaderCleanup::Headers(d.as_str())
            }
        })
        .collect()
}

/// The path to assert in `%check` for a wheel with nothing to import and no
/// shared library (NVIDIA's `nvcc`/`nvdisasm`: executables under `bin/`): the
/// shortest file `%install` leaves in site-packages, outside the dist-info.
fn wheel_payload_probe(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
    let cleanups = wheel_header_cleanups(layout);
    layout
        .files
        .iter()
        .filter(|f| {
            let top = f.split('/').next().unwrap_or("");
            !top.ends_with(".dist-info")
                && !top.ends_with(".data")
                && top != "__pycache__"
                && !f.ends_with(".pyc")
                && !cleanups.iter().any(|c| c.removes(f))
                && ap_ecosystem::sdist::is_shell_safe_member(f)
        })
        .min_by_key(|f| (f.len(), f.as_str()))
        .cloned()
}

/// The `%files` entries for a prebuilt wheel, from its own file list, or `None`
/// when the layout is not one this can list without guessing (a `.data`
/// directory, a top-level module, an odd file name): the review then fills the
/// section in. Convention 15 forbids globs, so every entry is an exact path or
/// a package directory.
///
///  * a directory with an `__init__.py` is a package: the directory is listed;
///  * a directory without one is a namespace shared with other wheels
///    (`nvidia/`, `cuda/`): it is `%dir`, and its children are listed;
///  * a directory that holds only files is listed as a whole (`nvidia/cu13/lib/`);
///  * what `%install` removes (bundled headers) is left out;
///  * the dist-info directory is listed with its licence files split off as
///    `%exclude` + `%license`, so each is owned once.
fn wheel_files_entries(
    layout: &ap_ecosystem::sdist::WheelLayout,
    sitedir: &str,
    foreign: &[String],
) -> Option<String> {
    use std::collections::BTreeMap;

    let dist_info = layout.dist_info.as_deref()?;
    let cleanups = wheel_header_cleanups(layout);
    let removed = |path: &str| cleanups.iter().any(|c| c.removes(path));
    let odd = |path: &str| {
        path.chars().any(|c| {
            c.is_whitespace() || matches!(c, '%' | '*' | '?' | '[' | ']' | '$' | '"' | '\'' | '\\')
        }) || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    };
    let mut payload: Vec<&str> = Vec::new();
    let mut licenses: Vec<&str> = Vec::new();
    for file in &layout.files {
        let file = file.as_str();
        if odd(file) {
            return None;
        }
        let top = file.split('/').next().unwrap_or("");
        // A top-level entry named like the dist-info directory has no member
        // path to slice; it falls to the top-level rule below and is not listed blind.
        if top == dist_info && file.contains('/') {
            let rel = &file[dist_info.len() + 1..];
            let leaf = rel.rsplit('/').next().unwrap_or(rel).to_ascii_uppercase();
            let is_license = rel.starts_with("licenses/")
                || ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|p| leaf.starts_with(p));
            if is_license {
                licenses.push(file);
            }
        } else if file.ends_with(".pth") && !file.contains('/') {
            // A top-level `.pth` is data for the interpreter, with no bytecode.
            payload.push(file);
        } else if top.ends_with(".data") || top == "__pycache__" || !file.contains('/') {
            // Scripts and data land outside site-packages; a top-level module
            // brings its own `__pycache__`: neither is listed blind.
            return None;
        } else if !removed(file) && !file.ends_with(".pyc") {
            payload.push(file);
        }
    }
    // A wheel with nothing but its dist-info (a metapackage: `cuda-toolkit`)
    // owns just that directory.

    // Directory tree of the payload: dir -> (direct files, child dirs).
    #[derive(Default)]
    struct Dir {
        files: Vec<String>,
        dirs: BTreeMap<String, Dir>,
    }
    let mut root = Dir::default();
    for path in &payload {
        let mut node = &mut root;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.dirs.entry(part.to_string()).or_default();
            } else {
                node.files.push(part.to_string());
            }
        }
    }
    fn emit(dir: &Dir, path: &str, sitedir: &str, foreign: &[String], out: &mut Vec<String>) {
        // A directory another package of the project also fills (convention
        // 83): co-owned with `%dir`, only this wheel's own files listed, never
        // the directory as a whole.
        let prefix = format!("{path}/");
        let shared = foreign.iter().any(|f| f.starts_with(&prefix));
        if !shared && (dir.files.iter().any(|f| f == "__init__.py") || dir.dirs.is_empty()) {
            out.push(format!("%{{{sitedir}}}/{path}/"));
            return;
        }
        out.push(format!("%dir %{{{sitedir}}}/{path}"));
        let mut files = dir.files.clone();
        files.sort();
        for f in &files {
            out.push(format!("%{{{sitedir}}}/{path}/{f}"));
        }
        // The build byte-compiles every module it installs: the `__pycache__`
        // beside modules listed one by one is installed too, and unowned unless
        // listed (a package directory above owns its own). In a shared
        // directory the sibling's bytecode lives there too, so list this
        // wheel's own modules' only.
        let own_py: Vec<&String> = files.iter().filter(|f| f.ends_with(".py")).collect();
        if !own_py.is_empty() {
            if shared {
                out.push(format!("%dir %{{{sitedir}}}/{path}/__pycache__"));
                for f in own_py {
                    let stem = f.trim_end_matches(".py");
                    out.push(format!("%{{{sitedir}}}/{path}/__pycache__/{stem}.*.pyc"));
                }
            } else {
                out.push(format!("%{{{sitedir}}}/{path}/__pycache__/"));
            }
        }
        for (name, child) in &dir.dirs {
            emit(child, &format!("{path}/{name}"), sitedir, foreign, out);
        }
    }
    let mut lines = Vec::new();
    root.files.sort();
    for f in &root.files {
        lines.push(format!("%{{{sitedir}}}/{f}"));
    }
    for (name, child) in &root.dirs {
        emit(child, name, sitedir, foreign, &mut lines);
    }
    licenses.sort();
    // Several files under `licenses/` are one entry: the directory.
    let licenses_dir = format!("{dist_info}/licenses/");
    if licenses.len() >= 2 && licenses.iter().all(|l| l.starts_with(&licenses_dir)) {
        lines.push(format!("%exclude %{{{sitedir}}}/{dist_info}/licenses"));
        lines.push(format!("%license %{{{sitedir}}}/{dist_info}/licenses"));
    } else {
        for l in &licenses {
            lines.push(format!("%exclude %{{{sitedir}}}/{l}"));
        }
        for l in &licenses {
            lines.push(format!("%license %{{{sitedir}}}/{l}"));
        }
    }
    lines.push(format!("%{{{sitedir}}}/{dist_info}/"));
    Some(lines.join("\n") + "\n")
}

/// The names of the `siblings` that own (hold the `__init__.py` of) a package
/// directory `layout` adds files to: this wheel completes their package, so it
/// needs them installed (convention 83 a). A directory this wheel owns itself is
/// not listed. Sorted, without duplicates.
fn shared_package_owners(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> Vec<String> {
    let mut owners: Vec<String> = Vec::new();
    for SiblingWheel { name, files, .. } in siblings {
        let owns = files.iter().any(|f| {
            let Some(dir) = f.strip_suffix("/__init__.py") else {
                return false;
            };
            let prefix = format!("{dir}/");
            let own_init = layout.files.iter().any(|m| m == f);
            !own_init && layout.files.iter().any(|m| m.starts_with(&prefix))
        });
        if owns && !owners.contains(name) {
            owners.push(name.clone());
        }
    }
    owners.sort();
    owners
}

/// Another wheel of the same project (convention 83): `name` is the
/// flavor-neutral RPM name, `files` what it installs relative to site-packages
/// (its dist-info left out), `purelib` its `Root-Is-Purelib` when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingWheel {
    pub name: String,
    pub files: Vec<String>,
    pub purelib: Option<bool>,
}

/// The top-level directories of `layout`'s payload (dist-info, `.data` and
/// bytecode directories left out).
fn payload_tops(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<&str> {
    let mut tops: Vec<&str> = layout
        .files
        .iter()
        .filter(|f| f.contains('/'))
        .filter_map(|f| f.split('/').next())
        .filter(|t| !t.ends_with(".dist-info") && !t.ends_with(".data") && *t != "__pycache__")
        .filter(|t| ap_ecosystem::sdist::is_shell_safe_member(t))
        .collect();
    tops.sort_unstable();
    tops.dedup();
    tops
}

/// Whether a sibling that declares `Root-Is-Purelib: true` fills a top-level
/// directory of `layout`: the whole tree then lives in `%{python_sitelib}`.
fn sibling_forces_purelib(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> bool {
    let tops = payload_tops(layout);
    siblings.iter().any(|s| {
        s.purelib == Some(true)
            && s.files
                .iter()
                .any(|f| f.split_once('/').is_some_and(|(t, _)| tops.contains(&t)))
    })
}

/// The `%install` lines that move a wheel installed into `%{python_sitearch}`
/// (it says `Root-Is-Purelib: false`) into `%{python_sitelib}`, beside the
/// sibling wheels that fill the same tree.
fn sitelib_relocation(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut text = String::from(
        "# the wheel says Root-Is-Purelib: false, but a sibling wheel of this project (a\n\
         # noarch package) fills the same tree in sitelib, and a package tree split\n\
         # between lib and lib64 cannot be imported: install it beside them (convention 83)\n\
         %python_expand mkdir -p %{buildroot}%{$python_sitelib}\n",
    );
    let mut tops: Vec<&str> = payload_tops(layout);
    if let Some(d) = layout.dist_info.as_deref()
        && ap_ecosystem::sdist::is_shell_safe_member(d)
    {
        tops.push(d);
    }
    for top in tops {
        text.push_str(&format!(
            "%python_expand mv %{{buildroot}}%{{$python_sitearch}}/{top} %{{buildroot}}%{{$python_sitelib}}/\n"
        ));
    }
    text
}

/// `spec`, generated for a wheel before a sibling wheel that fills the same
/// tree in `%{python_sitelib}` was known (convention 83), moved onto that one
/// tree: python-tokenspeed-mla failed with `No module named 'cutlass.cute'`
/// because `-base` and `-cu12` had been generated into sitearch while `-core`
/// is noarch. Every `sitearch` of the spec becomes `sitelib` and the wheel's
/// tree is moved there right after the install ([`sitelib_relocation`]), as the
/// generator does when it knows the siblings. A spec that does not use
/// sitearch, or already relocates, is returned as is.
pub fn with_sibling_tree_in_sitelib(
    spec: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> String {
    if !sibling_forces_purelib(layout, siblings)
        || !spec.contains("python_sitearch")
        || spec.contains("mv %{buildroot}%{$python_sitearch}/")
    {
        return spec.to_string();
    }
    let tail = if spec.ends_with('\n') { "\n" } else { "" };
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        out.push(
            line.replace("%{$python_sitearch}", "%{$python_sitelib}")
                .replace("%{python_sitearch}", "%{python_sitelib}"),
        );
    }
    // After the last install line.
    let at = out.iter().rposition(|l| {
        let t = l.trim_start();
        t.starts_with("%pyproject_install")
            || (t.starts_with("%python") && t.contains("_pyproject_install"))
    });
    if let Some(at) = at {
        let reloc: Vec<String> = sitelib_relocation(layout)
            .lines()
            .map(str::to_string)
            .collect();
        for (k, l) in reloc.into_iter().enumerate() {
            out.insert(at + 1 + k, l);
        }
    } else {
        return spec.to_string();
    }
    out.join("\n") + tail
}

/// The other wheels of `pkg_name`'s project, from every source that knows what
/// they install, in a fixed order so the result does not depend on which was
/// built first:
///  1. the wheel file in the sibling's own checkout (`<checkouts>/python-<name>/
///     *.whl`, the first by name), listed from the zip: present as soon as that
///     package's spec was generated, built or not;
///  2. otherwise the RPM already built in `rpms_dir`, listed with `rpm -qlp`.
///
/// A sibling is a package whose name shares all but the last two dash-separated
/// words (at least two) with `pkg_name`: `nvidia-cutlass-dsl-libs-base`/`-core`/
/// `-cu13`. Without any source the result is empty.
pub fn sibling_wheels(
    rpms_dir: &std::path::Path,
    checkouts: Option<&std::path::Path>,
    pkg_name: &str,
) -> Vec<SiblingWheel> {
    let mut out: Vec<SiblingWheel> = checkouts
        .map(|c| sibling_wheels_in_checkouts(c, pkg_name))
        .unwrap_or_default();
    for (name, files, purelib) in sibling_rpm_files(rpms_dir, pkg_name) {
        if !out.iter().any(|s| s.name == name) {
            out.push(SiblingWheel {
                name,
                files,
                purelib,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The siblings among the `python-*` package checkouts in `checkouts` that hold
/// a wheel (see [`sibling_wheels`]).
fn sibling_wheels_in_checkouts(checkouts: &std::path::Path, pkg_name: &str) -> Vec<SiblingWheel> {
    let Ok(entries) = std::fs::read_dir(checkouts) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let dir = e.file_name().to_string_lossy().into_owned();
        let Some(name) = dir.strip_prefix("python-") else {
            continue;
        };
        let name = name.to_lowercase();
        if !is_sibling_project(&name, pkg_name) {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(e.path()) else {
            continue;
        };
        let mut wheels: Vec<std::path::PathBuf> = inner
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "whl") && p.is_file())
            .collect();
        wheels.sort();
        let Some(wheel) = wheels.first() else {
            continue;
        };
        // Names only: reading a full layout would open the libraries of an
        // NVIDIA wheel, and this runs for every sibling.
        let Some(names) = ap_ecosystem::sdist::wheel_file_names(wheel) else {
            continue;
        };
        let files: Vec<String> = names
            .into_iter()
            .filter(|f| {
                f.split_once('/')
                    .is_some_and(|(top, _)| !top.ends_with(".dist-info"))
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        out.push(SiblingWheel {
            name: format!("python-{name}"),
            files,
            purelib: ap_ecosystem::sdist::wheel_root_is_purelib(wheel),
        });
    }
    out
}

/// The RPMs in `rpms_dir` built for the other wheels of `pkg_name`'s project,
/// as `(flavor-neutral name, files relative to site-packages, purelib)`, one
/// flavor per package. Asking `rpm` for the file lists is the only way to know
/// what a sibling really installs once built; without `rpm` the result is empty.
fn sibling_rpm_files(
    rpms_dir: &std::path::Path,
    pkg_name: &str,
) -> Vec<(String, Vec<String>, Option<bool>)> {
    let Ok(entries) = std::fs::read_dir(rpms_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            sibling_rpm_name(&file, pkg_name).map(|_| (file, e.path()))
        })
        .collect();
    paths.sort();
    let mut out: Vec<(String, Vec<String>, Option<bool>)> = Vec::new();
    for (file, path) in paths {
        let Some(name) = sibling_rpm_name(&file, pkg_name) else {
            continue;
        };
        if out.iter().any(|(n, _, _)| *n == name) {
            continue;
        }
        let Ok(listing) = std::process::Command::new("rpm")
            .args(["-qlp", "--nosignature"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        if !listing.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&listing.stdout);
        let files: Vec<String> = site_package_files(&text)
            .into_iter()
            .filter(|f| {
                f.split('/')
                    .next()
                    .is_some_and(|t| !t.ends_with(".dist-info"))
                    && f.contains('/')
            })
            .collect();
        if !files.is_empty() {
            let purelib = if text.contains("/usr/lib64/") {
                Some(false)
            } else if text.contains("/usr/lib/python") {
                Some(true)
            } else {
                None
            };
            out.push((name, files, purelib));
        }
    }
    out
}

/// Whether the (lowercase, dash-separated) package `name` is another wheel of
/// the project `pkg_name` belongs to: not `pkg_name` itself, and sharing all
/// but the last two words with it, at least two.
fn is_sibling_project(name: &str, pkg_name: &str) -> bool {
    let own = pkg_name.to_lowercase().replace('_', "-");
    if name == own || name.ends_with("-debuginfo") || name.ends_with("-debugsource") {
        return false;
    }
    let a: Vec<&str> = name.split('-').collect();
    let b: Vec<&str> = own.split('-').collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let need = a.len().min(b.len()).saturating_sub(2).max(2);
    common >= need
}

/// The flavor-neutral package name of the RPM file `file` when it belongs to a
/// sibling wheel project of `pkg_name` (see [`sibling_wheels`]); `None`
/// for another project, for `pkg_name` itself, and for a source or debug RPM.
fn sibling_rpm_name(file: &str, pkg_name: &str) -> Option<String> {
    let stem = file.strip_suffix(".rpm")?;
    if stem.ends_with(".src") || stem.ends_with(".nosrc") {
        return None;
    }
    let rest = stem.strip_prefix("python3")?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let base = rest[digits..].strip_prefix('-')?;
    // `<name>-<version>-<release>.<arch>`: cut the last two dashes.
    let mut parts = base.rsplitn(3, '-');
    let (_release, _version, name) = (parts.next()?, parts.next()?, parts.next()?);
    let name = name.to_lowercase();
    is_sibling_project(&name, pkg_name).then(|| format!("python-{name}"))
}

/// The paths below `site-packages` in an `rpm -ql` listing, for files only
/// (directory lines end the same way, so a path that is a prefix of another is a
/// directory and dropped).
fn site_package_files(listing: &str) -> Vec<String> {
    let mut paths: Vec<String> = listing
        .lines()
        .filter_map(|l| {
            l.split_once("/site-packages/")
                .map(|(_, p)| p.trim().to_string())
        })
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    let all = paths.clone();
    paths.retain(|p| !all.iter().any(|q| q.starts_with(&format!("{p}/"))));
    paths
}

#[cfg(test)]
mod shared_wheel_tests;

/// The four real wheels of NVIDIA's `nvidia-cutlass-dsl-libs-*` 4.7.1 (convention
/// 83): one namespace tree `nvidia_cutlass_dsl/` filled by `-base` (compiled MLIR
/// half, `Root-Is-Purelib: false`), `-core` (pure Python, `true`) and the
/// `-cu12`/`-cu13` flavors (a `.so` each, `false`). The file lists are the
/// wheels' own, copied from the zips.
#[cfg(test)]
mod cutlass_split_wheels_tests;

#[cfg(test)]
mod adopt_sources_tests;

/// `text` for a spec tag or description: one line, every `%` doubled so rpm does
/// not expand a macro out of upstream's wording.
fn rpm_text(text: &str) -> String {
    text.replace('%', "%%")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `%description` body of a wheel spec: the METADATA's first paragraph,
/// wrapped, with every `%` doubled (rpm expands macros in a description), or the
/// summary when the wheel has none.
fn wheel_description(paragraph: Option<&str>, summary: &str) -> String {
    let text = paragraph
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(summary);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.replace('%', "%%").split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 76 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines.join("\n")
}

/// Whether `file` (a path inside a wheel) is a licence text: a licence-named
/// leaf anywhere, or anything under `<dist-info>/licenses/`.
fn is_license_path(file: &str) -> bool {
    let leaf = file.rsplit('/').next().unwrap_or(file).to_ascii_uppercase();
    file.contains(".dist-info/licenses/")
        || ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| leaf.starts_with(p))
}

/// Whether the wheel ships a licence text of its own.
pub fn wheel_has_license_file(layout: &ap_ecosystem::sdist::WheelLayout) -> bool {
    layout.files.iter().any(|f| is_license_path(f))
}

/// The leaf name and bytes of the licence file at the top of a `.tar.gz` sdist
/// (`LICENSE`, `LICENSE.txt`, `COPYING`; the shortest name wins), at most 1 MiB.
pub fn sdist_top_level_license(tarball: &std::path::Path) -> Option<(String, Vec<u8>)> {
    use std::io::Read;
    let file = std::fs::File::open(tarball).ok()?;
    let mut archive = ap_ecosystem::sdist::bounded_tar_gz(file);
    let mut best: Option<(String, Vec<u8>)> = None;
    for entry in archive
        .entries()
        .ok()?
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
    {
        let Ok(entry) = entry else { continue };
        if !entry.header().entry_type().is_file() || entry.size() > (1 << 20) {
            continue;
        }
        let Ok(path) = entry.path() else { continue };
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let [_top, leaf] = parts.as_slice() else {
            continue;
        };
        let upper = leaf.to_ascii_uppercase();
        let plain = leaf
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        let named = ["LICENSE", "LICENCE", "COPYING"]
            .iter()
            .any(|p| upper.starts_with(p));
        if !plain || !named || upper.ends_with(".PY") {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _)| b.len() <= leaf.len()) {
            continue;
        }
        let leaf = leaf.clone();
        let mut bytes = Vec::new();
        if entry.take(1 << 20).read_to_end(&mut bytes).is_ok() {
            best = Some((leaf, bytes));
        }
    }
    best
}

/// A wheel with no licence text of its own, whose sdist has one: copy it from
/// the sdist in `dir` (the checkout) next to the wheel and return its file name,
/// for [`with_license_source`]. `None` when the wheel has one, there is no sdist
/// there, or the sdist has none.
pub fn copy_sdist_license(
    dir: &std::path::Path,
    version: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
) -> Option<String> {
    if wheel_has_license_file(layout) {
        return None;
    }
    let mut tarballs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.ends_with(".tar.gz") && n.contains(version))
        })
        .collect();
    tarballs.sort();
    let (name, bytes) = tarballs.iter().find_map(|t| sdist_top_level_license(t))?;
    std::fs::write(dir.join(&name), bytes).ok()?;
    Some(name)
}

/// `spec` of a prebuilt wheel with the licence `file` (copied from the sdist by
/// [`copy_sdist_license`]) as `Source1`, copied to the build directory in
/// `%prep` and owned as `%license`, for a wheel that carries no licence text.
/// Left alone when the spec has a `%license` or a `Source1` already.
pub fn with_license_source(spec: &str, file: &str) -> String {
    if spec.contains("%license") || spec.contains("Source1:") {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        let t = line.trim_end();
        if t.starts_with("Source0:") {
            out.push_str(&format!(
                "# the wheel has no licence text; this one is the sdist's\nSource1:        {file}\n"
            ));
        } else if t == "%prep" {
            out.push_str(&format!("cp -p %{{SOURCE1}} {file}\n"));
        } else if t.starts_with("%files") && t.contains("%{python_files}") {
            out.push_str(&format!("%license {file}\n"));
        }
    }
    out
}

#[cfg(test)]
mod sdist_license_tests;

/// `spec` with a note under `%prep` that the release has no sdist and no source
/// repository, so the unpacked wheel is all there is. Only for a true no-sdist
/// release; a package switched to its wheel after a failed source build has a
/// sdist and gets no such note.
pub fn with_no_sdist_note(spec: &str) -> String {
    const NOTE: &str = "# this release has no sdist and no source repository: the upstream wheel is\n\
                        # installed as it is, so check the architecture and the file list against it\n";
    let mut out = String::with_capacity(spec.len() + NOTE.len());
    let mut done = false;
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.trim_end() == "%prep" {
            out.push_str(NOTE);
            done = true;
        }
    }
    out
}

/// Whether a `Requires-Python` specifier excludes a flavor the project builds
/// through an upper bound (`<3.13`, `<=3.12`, `==3.12.*`): pip then refuses to
/// install the wheel for that flavor unless told otherwise.
pub fn requires_python_caps_below(requires_python: &str, flavors: &[String]) -> bool {
    let versions: Vec<(u32, u32)> = flavors.iter().filter_map(|f| flavor_version(f)).collect();
    let parse = |v: &str| -> Option<(u32, u32)> {
        let mut it = v.trim().trim_end_matches(".*").split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().map_or(Some(0), |m| m.parse().ok())?;
        Some((major, minor))
    };
    requires_python.split(',').map(str::trim).any(|clause| {
        if let Some(v) = clause.strip_prefix("<=") {
            // `<=3.12` still allows 3.12.x; `<=3.12.5` allows all of 3.12.
            parse(v).is_some_and(|cap| versions.iter().any(|f| *f > cap))
        } else if let Some(v) = clause.strip_prefix('<') {
            let patch = v.trim().split('.').count() > 2;
            parse(v).is_some_and(|cap| {
                versions
                    .iter()
                    .any(|f| if patch { *f > cap } else { *f >= cap })
            })
        } else if let Some(v) = clause.strip_prefix("==") {
            v.trim().ends_with(".*")
                && parse(v).is_some_and(|only| versions.iter().any(|f| *f != only))
        } else {
            false
        }
    })
}

/// `spec` with `export PIP_IGNORE_REQUIRES_PYTHON=1` as the first line of `%build`
/// and `%install` (each is its own shell and each may run pip), when the wheel's
/// `Requires-Python` has an upper bound below a flavor that is built. A spec that
/// already sets the variable is returned as it is.
pub fn with_pip_ignore_requires_python(
    spec: &str,
    requires_python: Option<&str>,
    flavors: &[String],
) -> String {
    if spec.contains("PIP_IGNORE_REQUIRES_PYTHON")
        || !requires_python.is_some_and(|r| requires_python_caps_below(r, flavors))
    {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 160);
    for line in spec.split_inclusive('\n') {
        out.push_str(line);
        if matches!(line.trim_end(), "%build" | "%install") {
            out.push_str(
                "# the wheel caps Requires-Python below a Python we build for\n\
                 export PIP_IGNORE_REQUIRES_PYTHON=1\n",
            );
        }
    }
    out
}

/// SPDX id for an OSI classifier name, if it is one we know for certain.
fn classifier_spdx(name: &str) -> Option<&'static str> {
    Some(match name {
        "MIT License" => "MIT",
        "Apache Software License" => "Apache-2.0",
        "BSD License" => return None, // 2- vs 3-clause is not recoverable
        "ISC License (ISCL)" => "ISC",
        "Mozilla Public License 2.0 (MPL 2.0)" => "MPL-2.0",
        "GNU General Public License v2 (GPLv2)" => "GPL-2.0-only",
        "GNU General Public License v2 or later (GPLv2+)" => "GPL-2.0-or-later",
        "GNU General Public License v3 (GPLv3)" => "GPL-3.0-only",
        "GNU General Public License v3 or later (GPLv3+)" => "GPL-3.0-or-later",
        "GNU Lesser General Public License v3 (LGPLv3)" => "LGPL-3.0-only",
        "GNU Lesser General Public License v3 or later (LGPLv3+)" => "LGPL-3.0-or-later",
        "GNU Affero General Public License v3" => "AGPL-3.0-only",
        "Python Software Foundation License" => "PSF-2.0",
        "The Unlicense (Unlicense)" => "Unlicense",
        _ => return None,
    })
}

/// The `License:` value a wheel's own METADATA supports, or `None`: the SPDX
/// `License-Expression`, else the `License ::` classifiers that map to one SPDX
/// id, else a free-text `License:` that is a known SPDX id or spelling of one
/// (`MIT`, `Apache 2.0`, `3-clause BSD`) or NVIDIA's proprietary redistribution
/// text. Anything else stays `FIXME` for a person: a guessed licence is worse
/// than a flagged one.
fn wheel_license(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
    if let Some(expr) = &layout.license_expression {
        return Some(spdx_expression_for_spec(expr));
    }
    if !layout.license_classifiers.is_empty() {
        let tag = license_tag(&License::Classifiers(layout.license_classifiers.clone()));
        if tag != "FIXME" {
            return Some(tag);
        }
    }
    spdx_from_free_text(layout.license_text.as_deref()?)
}

/// An SPDX expression for `License:`: a vendor's own licence reference
/// (`LicenseRef-NVIDIA-Proprietary`) is not a SPDX id and becomes the non-free
/// tag; anything else is kept as declared.
fn spdx_expression_for_spec(expr: &str) -> String {
    // Only the term that names the vendor licence is replaced: the rest of an
    // expression (`MIT AND LicenseRef-NVIDIA-Proprietary`) stays.
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let low = word.to_ascii_lowercase();
        if low.contains("nvidia") || low.contains("proprietary") {
            out.push_str(NVIDIA_PROPRIETARY);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in expr.chars() {
        if c.is_whitespace() || c == '(' || c == ')' {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    // Two vendor terms are one. Dropping the second would leave its operator
    // dangling (`MIT AND SUSE-NonFree AND`), so the repeat is merged with the
    // operator that joins it instead.
    for op in ["AND", "OR"] {
        let twice = format!("{NVIDIA_PROPRIETARY} {op} {NVIDIA_PROPRIETARY}");
        while out.contains(&twice) {
            out = out.replace(&twice, NVIDIA_PROPRIETARY);
        }
    }
    out
}

/// The SPDX id a free-text licence name spells (`MIT`, `Apache 2.0`,
/// `3-clause BSD`), or the non-free tag for NVIDIA's proprietary redistribution
/// text; `None` for anything else.
fn spdx_from_free_text(text: &str) -> Option<String> {
    let key: String = text
        .to_ascii_lowercase()
        .replace(['_', ',', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let spdx = match key.as_str() {
        "mit" | "mit license" => "MIT",
        "mit-0" => "MIT-0",
        "apache 2 0"
        | "apache-2 0"
        | "apache license 2 0"
        | "apache license version 2 0"
        | "apache software license"
        | "apache software license 2 0"
        | "apache 2" => "Apache-2.0",
        "bsd-3-clause" | "bsd 3-clause" | "bsd 3 clause" | "3-clause bsd" | "3 clause bsd"
        | "new bsd" | "modified bsd" | "bsd-3" => "BSD-3-Clause",
        "bsd-2-clause" | "bsd 2-clause" | "bsd 2 clause" | "2-clause bsd" | "simplified bsd"
        | "freebsd" => "BSD-2-Clause",
        "isc" | "isc license" => "ISC",
        "mpl-2 0" | "mpl 2 0" | "mozilla public license 2 0" => "MPL-2.0",
        "psf" | "psf-2 0" | "python software foundation license" => "PSF-2.0",
        "unlicense" | "the unlicense" => "Unlicense",
        "zlib" => "Zlib",
        "0bsd" => "0BSD",
        _ => {
            let nvidia = key.contains("nvidia")
                && ["proprietary", "license agreement", "software license"]
                    .iter()
                    .any(|w| key.contains(w));
            return nvidia.then(|| NVIDIA_PROPRIETARY.to_string());
        }
    };
    Some(spdx.to_string())
}

/// The `License:` of a repackaged wheel: the release's own licence when it is a
/// usable tag, else what the wheel's METADATA supports (see [`wheel_license`]).
/// A free-text licence that is a known spelling is mapped to its SPDX id; one
/// that is prose or pasted licence text (multi-line, long) is never written into
/// the tag. A vendor's proprietary licence is the non-free tag.
fn wheel_declared_license(
    release: &Release,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    let own = match &release.license {
        License::FreeText(text) => {
            let text = text.trim();
            match spdx_from_free_text(text) {
                Some(id) => id,
                None if text.is_empty() || text.contains('\n') || text.len() > 100 => {
                    "FIXME".to_string()
                }
                None => text.to_string(),
            }
        }
        License::Spdx(expr) => spdx_expression_for_spec(expr),
        other => license_tag(other),
    };
    match own.as_str() {
        "FIXME" => layout.and_then(wheel_license).unwrap_or(own),
        _ => own,
    }
}

/// The licence of a GCC runtime library bundled in a wheel (torch's `libgomp`).
const GCC_RUNTIME_LICENSE: &str = "GPL-3.0-or-later WITH GCC-exception-3.1";

/// Licence terms a wheel's own `License:` does not carry because the wheel
/// bundles a third party's binaries: NVIDIA's toolchain programs and the CUPTI
/// libraries (triton declares MIT but ships `ptxas`) are non-free, and GCC's
/// `libgomp` is GPL with the runtime exception. In a fixed order, no
/// duplicates; found from the file names only.
fn wheel_vendor_license_terms(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<&'static str> {
    let mut nonfree = false;
    let mut gcc = false;
    for file in &layout.files {
        let top = file.split('/').next().unwrap_or("");
        if top.ends_with(".dist-info") || top.ends_with(".data") {
            continue;
        }
        let name = file.rsplit('/').next().unwrap_or(file);
        if matches!(
            name,
            "ptxas"
                | "ptxas-blackwell"
                | "cuobjdump"
                | "nvdisasm"
                | "nvlink"
                | "fatbinary"
                | "nvprune"
        ) || (name.starts_with("libcupti") && name.contains(".so"))
        {
            nonfree = true;
        }
        if name.starts_with("libgomp") && name.contains(".so") {
            gcc = true;
        }
    }
    let mut terms = Vec::new();
    if nonfree {
        terms.push(NVIDIA_PROPRIETARY);
    }
    if gcc {
        terms.push(GCC_RUNTIME_LICENSE);
    }
    terms
}

/// `license` with each of `terms` it does not already name, joined by `AND`
/// (an `OR` expression is parenthesised first). A licence still `FIXME` is left
/// for a person: terms beside an unknown are noise.
fn with_license_terms(license: String, terms: &[&str]) -> String {
    if license == "FIXME" {
        return license;
    }
    let mut out = license;
    for term in terms {
        if out.contains(term) {
            continue;
        }
        // `AND` binds tighter than `OR`: an expression with an `OR` outside any
        // parentheses has to be grouped before a term is added to all of it.
        if has_top_level_or(&out) {
            out = format!("({out})");
        }
        out = format!("{out} AND {term}");
    }
    out
}

/// Whether an SPDX expression has an `OR` outside every pair of parentheses
/// (`MIT OR Apache-2.0`, `(A OR B) AND C OR D`; not `(A OR B)`).
fn has_top_level_or(expr: &str) -> bool {
    let mut depth = 0i32;
    let mut word = String::new();
    let mut found = false;
    for c in expr.chars().chain(std::iter::once(' ')) {
        match c {
            '(' | ')' => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
                depth += if c == '(' { 1 } else { -1 };
            }
            c if c.is_whitespace() => {
                if depth == 0 && word == "OR" {
                    found = true;
                }
                word.clear();
            }
            c => word.push(c),
        }
    }
    found
}

fn license_tag(license: &License) -> String {
    match license {
        License::Spdx(expr) => expr.clone(),
        License::FreeText(text) => text.clone(),
        License::Classifiers(classifiers) => {
            // Only classifiers with a known SPDX id are emitted: OSI display
            // names ("MIT License") are not valid in `License:`. When nothing
            // maps, `FIXME` is what the spec validator and agent already flag,
            // rather than an empty tag that looks like a finished spec.
            let ids: Vec<&str> = classifiers
                .iter()
                .filter_map(|c| c.strip_prefix("License :: OSI Approved :: "))
                .filter_map(classifier_spdx)
                .collect();
            if ids.is_empty() {
                "FIXME".to_string()
            } else {
                ids.join(" AND ")
            }
        }
        License::Unknown => "FIXME".to_string(),
    }
}

/// Whether a wheel's declared licence is a vendor's proprietary one rather than
/// an open source licence: free text or an SPDX expression naming NVIDIA or a
/// proprietary / end-user agreement, or the "Proprietary" trove classifier.
pub fn license_is_proprietary(license: &License) -> bool {
    let text = match license {
        License::Spdx(t) | License::FreeText(t) => t.to_lowercase(),
        License::Classifiers(c) => c.join(" ").to_lowercase(),
        License::Unknown => return false,
    };
    [
        "proprietary",
        "nvidia",
        "license agreement",
        "eula",
        "nonfree",
    ]
    .iter()
    .any(|w| text.contains(w))
}

/// `spec` with a sentence added to the `%description`, saying the package is a
/// vendor's prebuilt library unpacked from its wheel and not built from source.
pub fn note_prebuilt_library(spec: &str, proprietary: bool) -> String {
    let note = if proprietary {
        "This package redistributes the vendor's prebuilt binary library, unpacked from \
         its wheel; it is not built from source and its licence is not an open source one."
    } else {
        "This package redistributes the upstream prebuilt binary library, unpacked from \
         its wheel; it is not built from source."
    };
    let lines: Vec<&str> = spec.lines().collect();
    let insert_after = description_paragraph_end(&lines);
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        out.push(line.to_string());
        if Some(idx) == insert_after {
            out.push(String::new());
            out.push(note.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Index of the last line of the first paragraph of the `%description` body: the
/// line before the blank line or the next section header that ends it. With no
/// text under the header, the header itself. `None` without a `%description`.
fn description_paragraph_end(lines: &[&str]) -> Option<usize> {
    let header = lines.iter().position(|l| l.trim_end() == "%description")?;
    let mut end = header;
    for (idx, line) in lines.iter().enumerate().skip(header + 1) {
        let t = line.trim();
        if t.is_empty() {
            if end > header {
                break;
            }
            continue;
        }
        // `%{...}` is a macro in the text, not a section header.
        if t.starts_with('%') && !t.starts_with("%{") {
            break;
        }
        end = idx;
    }
    Some(end)
}

#[cfg(test)]
mod prebuilt_library_tests;

/// Runtime dependency tags that must come before `%python_subpackages`: after
/// it they belong to no subpackage (or end up in the description).
const DEPENDENCY_TAGS: &[&str] = &[
    "Requires",
    "Recommends",
    "Suggests",
    "Supplements",
    "Conflicts",
    "Obsoletes",
    "Provides",
];

fn is_dependency_tag_line(line: &str) -> bool {
    let t = line.trim_start();
    DEPENDENCY_TAGS.iter().any(|tag| {
        t.strip_prefix(tag).is_some_and(|rest| {
            let rest = rest
                .strip_prefix('(')
                .and_then(|r| r.split_once(')'))
                .map(|(_, after)| after)
                .unwrap_or(rest);
            rest.starts_with(':')
        })
    })
}

/// A runtime `Requires:`/`Recommends:` written with `%{python_module X}` names
/// every flavor's package, so the python314 RPM requires python313-X as well.
/// The runtime form is flavor-neutral, `python-X`, which `%python_subpackages`
/// rewrites per flavor; `%{python_module}` is for `BuildRequires`. Rewrites the
/// simple forms (`%{python_module name}` with an optional version bound) and
/// leaves conditional ones alone. Idempotent.
pub fn normalize_runtime_python_module(spec: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        let redist_line;
        let line = if crate::redist::enabled() {
            redist_line = redist_runtime_requires(line);
            redist_line.as_str()
        } else {
            line
        };
        let runtime = ["Requires", "Recommends", "Suggests", "Supplements"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if runtime && line.contains("%{python_module ") && !line.contains(" if ") {
            let mut rewritten = line.to_string();
            while let Some(start) = rewritten.find("%{python_module ") {
                let Some(end) = rewritten[start..].find('}') else {
                    break;
                };
                let inner = rewritten[start + "%{python_module ".len()..start + end].to_string();
                if inner.contains('%') || inner.contains('{') {
                    break;
                }
                rewritten.replace_range(start..start + end + 1, &format!("python-{inner}"));
            }
            out.push(rewritten);
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// With `--nvidia-redist`, a runtime `Requires:` of a CUDA wheel, written as
/// `%{python_module nvidia-cublas >= 13.1}` or `python-nvidia-cublas`, names the
/// package built from NVIDIA's archive instead: `cuda-cublas`, with no version
/// (the wheel's scheme is not the component's). Other lines are returned as they
/// are.
fn redist_runtime_requires(line: &str) -> String {
    static MODULE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static PLAIN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let t = line.trim_start();
    let runtime = ["Requires", "Recommends", "Suggests", "Supplements"]
        .iter()
        .any(|tag| {
            t.strip_prefix(tag)
                .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
        });
    if !runtime || line.contains(" if ") {
        return line.to_string();
    }
    let module = MODULE.get_or_init(|| {
        regex::Regex::new(r"%\{python_module\s+([^\s}]+)(?:\s*(?:[<>=!~]+)\s*[^\s}]+)?\}").unwrap()
    });
    let plain = PLAIN.get_or_init(|| {
        regex::Regex::new(r"(^|[\s:])python-([^\s,]+)(?:\s*(?:[<>=!~]+)\s*[^\s,]+)?").unwrap()
    });
    let out = module.replace_all(line, |c: &regex::Captures| {
        crate::redist::package_name(&c[1]).unwrap_or_else(|| c[0].to_string())
    });
    plain
        .replace_all(
            &out,
            |c: &regex::Captures| match crate::redist::package_name(&c[2]) {
                Some(cuda) => format!("{}{cuda}", &c[1]),
                None => c[0].to_string(),
            },
        )
        .into_owned()
}

/// Dependency tags written after `%python_subpackages` (up to the next section)
/// moved above it.
pub fn hoist_dependency_tags(spec: &str) -> String {
    let lines: Vec<&str> = spec.lines().collect();
    let Some(sub) = lines.iter().position(|l| l.trim() == "%python_subpackages") else {
        return spec.to_string();
    };
    let mut moved: Vec<&str> = Vec::new();
    let mut kept_after: Vec<&str> = Vec::new();
    let mut in_preamble = true;
    for line in &lines[sub + 1..] {
        if in_preamble
            && line.trim_start().starts_with('%')
            && !line.trim_start().starts_with("%global")
            && !line.trim_start().starts_with("%define")
        {
            in_preamble = false;
        }
        if in_preamble && is_dependency_tag_line(line) {
            moved.push(line);
        } else {
            kept_after.push(line);
        }
    }
    if moved.is_empty() {
        return spec.to_string();
    }
    let mut out: Vec<&str> = lines[..sub].to_vec();
    out.extend(moved);
    out.push(lines[sub]);
    out.extend(kept_after);
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// The licence tag for NVIDIA's redistributable proprietary binaries: openSUSE
/// uses `SUSE-NonFree`, and rpmlint reports every `LicenseRef-...` spelling that
/// crept in (`LicenseRef-NVIDIA-Proprietary`, `LicenseRef-NvidiaProprietary`,
/// `NVIDIA Proprietary Software`) as `invalid-license`.
pub const NVIDIA_PROPRIETARY: &str = "SUSE-NonFree";

pub fn normalize_nvidia_license(spec: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        if let Some(rest) = line.strip_prefix("License:") {
            let mut value = rest.trim().to_string();
            for variant in [
                "LicenseRef-NVIDIA-Proprietary",
                "LicenseRef-NvidiaProprietary",
                "LicenseRef-Nvidia-Proprietary",
                "LicenseRef-NVIDIA-proprietary",
                "NVIDIA Proprietary Software",
                "LicenseRef-NVIDIAProprietary",
            ] {
                value = value.replace(variant, NVIDIA_PROPRIETARY);
            }
            out.push(format!("License:        {value}"));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// A second `%changelog` line (the file ends `%changelog` twice) removed.
pub fn dedupe_changelog_header(spec: &str) -> String {
    let mut seen = false;
    let mut out: Vec<&str> = Vec::new();
    for line in spec.lines() {
        if line.trim_end() == "%changelog" {
            if seen {
                continue;
            }
            seen = true;
        }
        out.push(line);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// Style problems a reviewer would send back: an emoji, or a `%description`
/// that only repeats the `Summary:`. One line each, empty when the spec is fine.
pub fn description_problems(spec: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_emoji =
        |c: char| matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF);
    if body.chars().any(is_emoji) {
        problems.push("the spec contains an emoji (Summary, description or a comment)".to_string());
    }
    let summary = tag_value(body, "Summary:").map(|s| s.trim().to_string());
    let mut description = String::new();
    let mut in_desc = false;
    for line in body.lines() {
        let t = line.trim_end();
        if t.starts_with("%description") && (t == "%description" || t.starts_with("%description "))
        {
            if in_desc {
                break;
            }
            in_desc = true;
            continue;
        }
        if in_desc {
            if t.starts_with('%') {
                break;
            }
            if !t.is_empty() {
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(t.trim());
            }
        }
    }
    if let Some(summary) = summary
        && !summary.is_empty()
        && description.trim_end_matches('.') == summary.trim_end_matches('.')
    {
        problems.push(
            "the %description only repeats the Summary: say what the package does and \
             what it is for, in a sentence or two"
                .to_string(),
        );
    }
    problems
}

/// Whether the spec's `%files` names a `%license` (or `%doc` of a licence
/// file). Specs with no `%files` count as having one.
pub fn has_license_file_entry(spec: &str) -> bool {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    if !body.contains("%files") {
        return true;
    }
    body.lines().any(|l| l.trim_start().starts_with("%license"))
}

#[cfg(test)]
mod generator_tests;

#[cfg(test)]
mod skeleton_header_guard_tests;

#[cfg(test)]
mod expand_order_tests;

#[cfg(test)]
mod review_fix_tests;

/// The architectures `spec`'s top-level `ExclusiveArch:` allows when none of
/// them is `arch`, so a build for `arch` can only end `excluded`. `None` when the
/// spec has no such line, names a macro or `do_not_build` (a switch the tool
/// owns), has it inside a conditional, or allows `arch`.
pub fn exclusive_arch_excluding(spec: &str, arch: &str) -> Option<Vec<String>> {
    let mut depth = 0usize;
    let mut allowed: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        if t.starts_with("%description") || t.starts_with("%package") {
            break;
        }
        if t.starts_with("%if") {
            depth += 1;
        } else if t.starts_with("%endif") {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && t.len() > "exclusivearch:".len()
            && t[.."exclusivearch:".len()].eq_ignore_ascii_case("exclusivearch:")
        {
            allowed.extend(
                t["exclusivearch:".len()..]
                    .split_whitespace()
                    .map(str::to_string),
            );
        } else if depth > 0
            && t.get(..14)
                .is_some_and(|h| h.eq_ignore_ascii_case("exclusivearch:"))
        {
            return None;
        }
    }
    if allowed.is_empty()
        || allowed
            .iter()
            .any(|a| a.contains('%') || a == "do_not_build")
    {
        return None;
    }
    let is_x86_32 = |a: &str| matches!(a, "i386" | "i486" | "i586" | "i686");
    let hit = allowed
        .iter()
        .any(|a| a == arch || (is_x86_32(arch) && is_x86_32(a)));
    (!hit).then_some(allowed)
}

/// Lines of `spec` that still carry a `TODO` or `FIXME` placeholder, as `(line number,
/// text)`. The skeleton generators leave these for the agent to fill in, and a
/// spec that keeps one builds an empty or half-described package. The
/// `%changelog` is history, not a placeholder, and is not searched.
pub fn todo_placeholders(spec: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.contains("TODO") || line.contains("FIXME") {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// Lines of the spec that set a variable whose only purpose is to tell an
/// upstream build "I accept the licence terms" or "this is not a licence
/// violation" (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1`,
/// `ACCEPT_EULA=Y`). A guard like that exists because a person has to decide;
/// the review turn set torchcodec's to get its build past a wheel-state check
/// that protects against shipping FFmpeg builds with a different licence. Comments
/// and `%changelog` are not searched.
pub fn license_guard_overrides(spec: &str) -> Vec<(usize, String)> {
    use std::sync::LazyLock;
    static GUARD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b[A-Z0-9_]*(?:I_CONFIRM|NOT_A_LICENSE_VIOLATION|LICEN[SC]E_VIOLATION|ACCEPT_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|AGREE_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|ACCEPT_EULA)[A-Z0-9_]*\s*=",
        )
        .expect("static pattern")
    });
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if GUARD.is_match(line) {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// The comment a spec carries when a licence-confirmation variable was set
/// after checking what the guard asks about (see
/// [`unverified_license_guards`]).
pub const LICENSE_GUARD_EVIDENCE_TAG: &str = "licence-guard-verified:";

/// The guards of [`license_guard_overrides`] that are not backed by a check.
///
/// A guard that asks the builder to CONFIRM A FACT (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION`:
/// the FFmpeg linked is not GPL-licensed) is satisfied honestly by checking the
/// fact: the spec then carries a `# licence-guard-verified: <what was checked,
/// where>` comment of some substance, and the build goes ahead (a package that
/// cannot build is the worst result; a confirmation nobody checked is the other
/// one). A guard that asks to ACCEPT TERMS (`ACCEPT_EULA`, `AGREE_*_TERMS`) is a
/// person's decision, evidence or not, and is always returned.
pub fn unverified_license_guards(spec: &str) -> Vec<(usize, String)> {
    let evidence = spec
        .lines()
        .take_while(|l| l.trim_end() != "%changelog")
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .filter_map(|c| {
            let c = c.trim();
            let at = c.to_ascii_lowercase().find(LICENSE_GUARD_EVIDENCE_TAG)?;
            Some(c[at + LICENSE_GUARD_EVIDENCE_TAG.len()..].trim().len())
        })
        .any(|len| len >= 30);
    license_guard_overrides(spec)
        .into_iter()
        .filter(|(_, line)| {
            let upper = line.to_ascii_uppercase();
            let terms = ["EULA", "ACCEPT_", "AGREE_", "TERMS"]
                .iter()
                .any(|t| upper.contains(t));
            terms || !evidence
        })
        .collect()
}

#[cfg(test)]
mod sdist_files_tests;

#[cfg(test)]
mod license_guard_tests;

#[cfg(test)]
mod todo_placeholder_tests;

/// `spec` with its `Version:` set to `version`, keeping everything else: the
/// recipe a version bump carries forward. A `Version:` that is a single macro
/// (`%{package_version}`) is followed to its plain `%define`/`%global` and that
/// is set instead. `None` when the version cannot be found or is computed.
pub fn set_version(spec: &str, version: &str) -> Option<String> {
    let raw = tag_value(spec, "Version:")?;
    let macro_name = raw
        .strip_prefix("%{")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('%'));
    let defines = collect_simple_defines(spec);
    let (prefixes, old_value): (Vec<String>, String) = match macro_name {
        Some(name) if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') => {
            (
                vec![format!("%define {name}"), format!("%global {name}")],
                defines.get(name)?.clone(),
            )
        }
        Some(_) => return None,
        None if raw.contains('%') => return None,
        None => (vec!["Version:".to_string()], raw.to_string()),
    };
    let mut replaced = false;
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let matched = if replaced {
                None
            } else {
                prefixes.iter().find(|p| {
                    t.strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with([' ', '\t']) || rest.is_empty())
                })
            };
            match matched {
                Some(prefix) if old_value.trim().is_empty() => {
                    // No value to replace: the version goes where the value
                    // would start, after the tag's own padding (or one space).
                    replaced = true;
                    let indent = &line[..line.len() - t.len()];
                    let rest = &t[prefix.len()..];
                    let padding = &rest[..rest.len() - rest.trim_start().len()];
                    let padding = if padding.is_empty() { " " } else { padding };
                    format!("{indent}{prefix}{padding}{version}")
                }
                Some(_) if line.contains(old_value.as_str()) => {
                    replaced = true;
                    line.replacen(old_value.as_str(), version, 1)
                }
                _ => line.to_string(),
            }
        })
        .collect();
    replaced.then(|| lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod set_version_tests;

#[cfg(test)]
mod normalizer_tests;

/// Collapse the per-file licence lines under one `<name>.dist-info/licenses`
/// directory in `%files` (torch lists ~86 `licenses/third_party/...` files) into
/// the directory itself: `%exclude <dir>` beside `%license <dir>`, so the
/// dist-info entry that also matches them does not list them twice. Applies to a
/// directory with at least three such `%license` lines; the `%exclude` lines for
/// the same files are dropped with them.
pub fn collapse_dist_info_license_lines(spec: &str) -> String {
    const MIN_LINES: usize = 3;
    let dir_of = |line: &str| -> Option<String> {
        let rest = line
            .trim()
            .strip_prefix("%license")
            .or_else(|| line.trim().strip_prefix("%exclude"))?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let path = rest.trim();
        let (dir, _file) = path.split_once(".dist-info/licenses/")?;
        (!path.contains(char::is_whitespace)).then(|| format!("{dir}.dist-info/licenses"))
    };
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for l in spec.lines().filter(|l| l.trim().starts_with("%license")) {
        if let Some(d) = dir_of(l) {
            *counts.entry(d).or_default() += 1;
        }
    }
    counts.retain(|_, n| *n >= MIN_LINES);
    if counts.is_empty() {
        return spec.to_string();
    }
    let mut done: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        match dir_of(line) {
            Some(d) if counts.contains_key(&d) => {
                if !done.contains(&d) {
                    out.push(format!("%exclude {d}"));
                    out.push(format!("%license {d}"));
                    done.push(d);
                }
            }
            _ => out.push(line.to_string()),
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod license_collapse_tests;

/// The lines of the `%check` section of `spec`, empty when there is none.
fn check_section(spec: &str) -> Vec<&str> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        if in_check
            && t.starts_with('%')
            && [
                "%files",
                "%install",
                "%prep",
                "%build",
                "%package",
                "%description",
                "%post",
                "%pre",
                "%preun",
                "%postun",
                "%conf",
                "%generate_buildrequires",
            ]
            .iter()
            .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if in_check {
            out.push(line);
        }
    }
    out
}

/// Most `--ignore` options one `%check` may carry before it is a sign that the
/// suite is being cut down for a single root cause (a missing device, a missing
/// fixture directory) instead of replaced by an import smoke test.
pub const MAX_CHECK_IGNORES: usize = 6;

/// `%check` problems a reviewer sends back (convention 67): more than
/// [`MAX_CHECK_IGNORES`] `--ignore` options. Empty when fine.
pub fn check_quality_problems(spec: &str) -> Vec<String> {
    let ignores: usize = check_section(spec)
        .iter()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| {
            l.split_whitespace()
                .filter(|t| {
                    let t = t.trim_matches(|c| matches!(c, '\'' | '"'));
                    t == "--ignore"
                        || t == "--ignore-glob"
                        || t.starts_with("--ignore=")
                        || t.starts_with("--ignore-glob=")
                })
                .count()
        })
        .sum();
    if ignores > MAX_CHECK_IGNORES {
        return vec![format!(
            "its %check carries {ignores} `--ignore` options: a suite cut down this far has \
             one root cause (a device, a missing fixture directory, a network): find it and \
             select the runnable subset by marker or directory instead of ignoring files one \
             by one (convention 67). An import smoke test only with the quoted log line that \
             proves no subset can run (convention 11, rung iv)"
        )];
    }
    Vec::new()
}

/// Test paths (`tests/test_configs`, `test/data`) the `%check` runs or reads
/// that `exists` says the source does not have. A sdist often leaves the test
/// fixtures out (accelerate's `tests/test_configs`, brotlicffi's `testdata`):
/// the fix is the project's GitHub tag as a second source with the directory
/// extracted in `%prep` (convention 82 b). A path given to `--ignore` or
/// `--deselect` is not read, and one with a `%` or `$` is not a path.
pub fn missing_check_paths(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for line in check_section(spec) {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let mut skip_next = false;
        for tok in t.split_whitespace() {
            let tok = tok.trim_matches(|c| matches!(c, '\'' | '"'));
            // `--ignore tests/x` (the value in the next word) is not read either.
            if std::mem::take(&mut skip_next) {
                continue;
            }
            if matches!(tok, "--ignore" | "--ignore-glob" | "--deselect") {
                skip_next = true;
                continue;
            }
            let tok = tok.strip_prefix("./").unwrap_or(tok);
            let path = tok.split("::").next().unwrap_or(tok).trim_end_matches('/');
            let under_tests = ["tests/", "test/", "testdata/", "test_data/"]
                .iter()
                .any(|d| path.starts_with(d))
                && path.contains('/');
            let plain = path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
            if under_tests && plain && !path.contains("..") && !exists(path) {
                let path = path.to_string();
                if !missing.contains(&path) {
                    missing.push(path);
                }
            }
        }
    }
    missing
}

/// The review problem for [`missing_check_paths`], when there are any.
pub fn missing_check_path_problems(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let missing = missing_check_paths(spec, exists);
    // %prep already extracts a directory from a second source.
    if missing.is_empty() || spec.contains("--wildcards") {
        return Vec::new();
    }
    vec![format!(
        "its %check uses {} which the source archive does not ship: add the project's \
         GitHub tag archive as `Source1` (in a `_service` with `download_files`) and extract \
         the directory in %prep after the autosetup with `tar -xzf %{{SOURCE1}} \
         --strip-components=1 --wildcards '*/<dir>/*'` (convention 82 b); or drop the test \
         that needs it",
        missing.join(", ")
    )]
}

#[cfg(test)]
mod missing_check_path_tests;

/// Add the project's tag archive as a further source and extract `dirs` from it
/// in `%prep` (convention 82 b): a `SourceN: <url>#/<name>` line after the last
/// `Source` line, and `tar -xzf %{SOURCEN} --strip-components=1 --wildcards
/// '*/<dir>/*'` after the `%autosetup`/`%setup` of `%prep`. The `download_files`
/// service that fetches it is written by the pipeline when the file is missing.
/// `version` inside `url` becomes `%{version}`. `None` when the spec has no
/// `Source` line or no setup line in `%prep`, already extracts with
/// `--wildcards`, or a directory is not a plain relative path.
pub fn add_fixture_source(spec: &str, url: &str, version: &str, dirs: &[String]) -> Option<String> {
    let plain = |d: &str| {
        !d.is_empty()
            && !d.starts_with('/')
            && !d.contains("..")
            && d.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    };
    if dirs.is_empty()
        || !dirs.iter().all(|d| plain(d))
        || spec.contains("--wildcards")
        || url.contains(|c: char| c.is_whitespace() || c == '%' || c == '#')
    {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let source_num = |l: &str| -> Option<u32> {
        let rest = l.strip_prefix("Source")?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest[digits.len()..]
            .trim_start()
            .starts_with(':')
            .then(|| digits.parse().unwrap_or(0))
    };
    let mut last_source = None;
    let mut next = 0;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("%description") || l.starts_with("%package") {
            break;
        }
        if let Some(n) = source_num(l) {
            last_source = Some(i);
            next = next.max(n + 1);
        }
    }
    let last_source = last_source?;
    let prep = lines.iter().position(|l| l.trim() == "%prep")?;
    let mut setup = None;
    for (i, l) in lines.iter().enumerate().skip(prep + 1) {
        let t = l.trim();
        if t.starts_with('%')
            && !t.starts_with("%autosetup")
            && !t.starts_with("%setup")
            && !t.starts_with("%{")
            && !t.starts_with("%if")
            && !t.starts_with("%endif")
            && !t.starts_with("%else")
        {
            break;
        }
        if t.starts_with("%autosetup") || t.starts_with("%setup") {
            // The directive may continue on the next lines.
            let mut end = i;
            while lines[end].trim_end().ends_with('\\') && end + 1 < lines.len() {
                end += 1;
            }
            setup = Some(end);
            break;
        }
    }
    let setup = setup?;
    let url = if version.is_empty() {
        url.to_string()
    } else {
        url.replace(version, "%{version}")
    };
    let file = url.rsplit('/').next().unwrap_or("fixtures.tar.gz");
    let repo = url
        .split('/')
        .nth(4)
        .filter(|r| !r.is_empty())
        .unwrap_or("fixtures");
    let rename = format!("{repo}-fixtures-{file}");
    let wild: Vec<String> = dirs
        .iter()
        .map(|d| format!("'*/{}/*'", d.trim_end_matches('/')))
        .collect();
    let tar = format!(
        "tar -xzf %{{SOURCE{next}}} --strip-components=1 --wildcards {}",
        wild.join(" ")
    );
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
    for (i, l) in lines.iter().enumerate() {
        out.push((*l).to_string());
        if i == last_source {
            out.push(format!("Source{next}:        {url}#/{rename}"));
        }
        if i == setup {
            out.push(format!("# The sdist leaves out {}", dirs.join(", ")));
            out.push(tar.clone());
        }
    }
    let mut text = out.join("\n");
    if spec.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod add_fixture_source_tests;

/// [`add_fixture_source`] for the paths [`missing_check_paths`] reports: the
/// repository comes from the sdist metadata (`pkg_info`, `pyproject`), the tag
/// archive URL from `resolve(repo, version)`. `None` when there is nothing
/// missing, no repository, or no tag, so the reviewer prompt stays the fallback.
pub fn auto_fixture_source(
    spec: &str,
    exists: &dyn Fn(&str) -> bool,
    pkg_info: &str,
    pyproject: &str,
    version: &str,
    resolve: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let missing = missing_check_paths(spec, exists);
    if missing.is_empty() || spec.contains("--wildcards") {
        return None;
    }
    // One directory at the top of tests is enough when the missing path is a
    // file inside a fixtures directory already listed.
    let mut dirs: Vec<String> = Vec::new();
    for m in missing {
        if !dirs
            .iter()
            .any(|d| m == *d || m.starts_with(&format!("{d}/")))
        {
            dirs.retain(|d| !d.starts_with(&format!("{m}/")));
            dirs.push(m);
        }
    }
    let repo = ap_ecosystem::sdist::source_repo_from_metadata(pkg_info, pyproject)?;
    let url = resolve(&repo, version)?;
    add_fixture_source(spec, &url, version, &dirs)
}

#[cfg(test)]
mod auto_fixture_source_tests;

#[cfg(test)]
mod sharp_edge_tests;

/// Drop the `python-pytest` BuildRequires of a spec whose `%check` never runs
/// pytest (an import smoke test): the dependency only slows the build and
/// pulls plugins in. A spec with no `%check` section, or one that mentions
/// pytest, is left alone.
pub fn drop_unused_pytest_build_requires(spec: &str) -> String {
    let check = check_section(spec);
    if check.is_empty()
        || check
            .iter()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("pytest"))
    {
        return spec.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for line in spec.lines() {
        let t = line.trim();
        let is_pytest = t.starts_with("BuildRequires:")
            && t.trim_start_matches("BuildRequires:")
                .split_whitespace()
                .eq(["%{python_module", "pytest}"]);
        if !is_pytest {
            out.push(line);
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod check_quality_tests;

/// Whether the spec's `%check` does nothing but test that a file exists
/// (`test -e`, `test -x`, `ls`, also behind `%python_expand`): it has at least
/// one such command and no other. A package that ships Python modules is
/// imported instead (convention 67).
pub fn check_is_file_probe_only(spec: &str) -> bool {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut probes = 0;
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        const SECTIONS: &[&str] = &[
            "%files",
            "%changelog",
            "%install",
            "%prep",
            "%build",
            "%package",
            "%description",
            "%post",
            "%pre",
            "%preun",
            "%postun",
            "%conf",
            "%generate_buildrequires",
        ];
        if in_check
            && SECTIONS
                .iter()
                .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if !in_check || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let command = t.strip_prefix("%python_expand").map(str::trim).unwrap_or(t);
        if command.starts_with("test -e")
            || command.starts_with("test -x")
            || command.starts_with("test -f")
            || command.starts_with("ls ")
        {
            probes += 1;
        } else if t.starts_with("%if") || t.starts_with("%endif") || t.starts_with("%else") {
            continue;
        } else {
            return false;
        }
    }
    probes > 0
}

#[cfg(test)]
mod console_script_tests;

#[cfg(test)]
mod check_probe_tests;

/// Parse `op version` (`>= 0.1.10`, `< 0.1.12`, `= 2.0`) of one RPM dependency
/// item and say how `shipped` stands against it: `Some(None)` when it meets it,
/// `Some(Some(true))` when it is too new, `Some(Some(false))` when too old.
/// `None` when it is not a constraint this reads.
pub(crate) fn rpm_constraint_miss(
    constraint: &str,
    shipped: &pep440_rs::Version,
) -> Option<Option<bool>> {
    use std::str::FromStr;
    let c = constraint.trim();
    let (op, rest) = ["<=", ">=", "==", "<", ">", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim())))?;
    // An RPM version may carry an epoch or a release: only the upstream part.
    let ver = rest.split(['-', ' ']).next().unwrap_or(rest);
    let ver = ver.rsplit_once(':').map(|(_, v)| v).unwrap_or(ver);
    let wanted = pep440_rs::Version::from_str(ver).ok()?;
    let met = match op {
        "<=" => shipped <= &wanted,
        ">=" => shipped >= &wanted,
        "<" => shipped < &wanted,
        ">" => shipped > &wanted,
        _ => shipped == &wanted,
    };
    if met {
        return Some(None);
    }
    Some(Some(match op {
        "<=" | "<" => true,
        ">=" | ">" => false,
        _ => shipped > &wanted,
    }))
}

/// The dependency lines of `spec` for `dependency` with every version
/// constraint the version `shipped` misses removed, the name kept: a package the
/// distro ships is used as it is, and the bound that excludes it (an upstream
/// `<= 0.1.11`, an exact `==` pin) is dropped instead of the dependency
/// (convention 59). Constraints it meets stay. Duplicate lines this leaves are
/// merged.
pub fn relax_dependency_bounds(spec: &str, dependency: &str, shipped: &str) -> String {
    use std::str::FromStr;
    let Ok(shipped_v) = pep440_rs::Version::from_str(shipped.trim()) else {
        return spec.to_string();
    };
    let wanted = crate::osc::fuzzy_key(dependency);
    let mut out: Vec<String> = Vec::new();
    // One dedupe set per conditional branch: the same text in the `%if` and the
    // `%else` branch is two different requirements, not a duplicate.
    let mut seen_stack: Vec<std::collections::HashSet<String>> = vec![Default::default()];
    const OPEN_CONDITIONALS: &[&str] = &["%if", "%ifarch", "%ifnarch", "%ifos", "%ifnos"];
    const BRANCH_CONDITIONALS: &[&str] = &[
        "%elif",
        "%elifarch",
        "%elifnarch",
        "%elifos",
        "%elifnos",
        "%else",
    ];
    for line in spec.lines() {
        let t = line.trim_start();
        let keyword = t.split_whitespace().next().unwrap_or_default();
        if OPEN_CONDITIONALS.contains(&keyword) {
            seen_stack.push(Default::default());
            out.push(line.to_string());
            continue;
        }
        if BRANCH_CONDITIONALS.contains(&keyword) {
            if let Some(top) = seen_stack.last_mut() {
                top.clear();
            }
            out.push(line.to_string());
            continue;
        }
        if keyword == "%endif" {
            if seen_stack.len() > 1 {
                seen_stack.pop();
            }
            out.push(line.to_string());
            continue;
        }
        let is_dep_line = ["BuildRequires", "Requires", "Recommends", "Suggests"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if !is_dep_line {
            out.push(line.to_string());
            continue;
        }
        let (head, value) = line.split_once(':').unwrap_or((line, ""));
        let items: Vec<String> = value
            .split(',')
            .map(|item| {
                let item = item.trim();
                // `%{python_module name >= v}` or `python-name >= v`.
                let (open, inner, close) = match item
                    .strip_prefix("%{python_module ")
                    .and_then(|r| r.strip_suffix('}'))
                {
                    Some(inner) => ("%{python_module ", inner, "}"),
                    None => ("", item, ""),
                };
                let name_end = inner
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '='))
                    .unwrap_or(inner.len());
                let name = &inner[..name_end];
                let key = crate::osc::fuzzy_key(
                    name.strip_prefix("python-")
                        .or_else(|| name.strip_prefix("python3-"))
                        .unwrap_or(name),
                );
                let key_plain = crate::osc::fuzzy_key(name);
                if key != wanted && key_plain != wanted {
                    return item.to_string();
                }
                let constraint = inner[name_end..].trim();
                if constraint.is_empty() || constraint.contains(" if ") {
                    return item.to_string();
                }
                // `%{python_module trio >= 0.25.1 with %python-trio < 0.34}`:
                // each clause is judged on its own, the ones met stay.
                if !open.is_empty() && constraint.contains(" with ") {
                    let clauses: Vec<&str> = constraint.split(" with ").collect();
                    let kept: Vec<&str> = clauses
                        .iter()
                        .map(|c| c.trim_start_matches(|ch: char| !matches!(ch, '<' | '>' | '=')))
                        .map(str::trim)
                        .filter(|c| !matches!(rpm_constraint_miss(c, &shipped_v), Some(Some(_))))
                        .collect();
                    return if kept.len() == clauses.len() {
                        item.to_string()
                    } else if kept.is_empty() {
                        format!("{open}{name}{close}")
                    } else {
                        format!(
                            "{open}{name} {}{close}",
                            kept.join(&format!(" with %python-{name} "))
                        )
                    };
                }
                match rpm_constraint_miss(constraint, &shipped_v) {
                    Some(Some(_)) => format!("{open}{name}{close}"),
                    _ => item.to_string(),
                }
            })
            .collect();
        let rebuilt = format!(
            "{head}:{}{}",
            if value.starts_with(' ') || value.starts_with('\t') {
                &value[..value.len() - value.trim_start().len()]
            } else {
                " "
            },
            items.join(", ")
        );
        // A line that reads the same as one already kept in this branch is not
        // repeated.
        let seen = seen_stack
            .last_mut()
            .expect("the base scope is never popped");
        if rebuilt != line && !seen.insert(rebuilt.trim().to_string()) {
            continue;
        }
        seen.insert(rebuilt.trim().to_string());
        out.push(rebuilt);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// [`relax_dependency_bounds`] for each `(dependency, shipped version)` a
/// package's state records (`PkgState::relaxed_dependencies`): applied on every
/// validation, root package included, before anything is built.
pub fn relax_dependencies(spec: &str, relaxed: &[(String, String)]) -> String {
    relaxed
        .iter()
        .fold(spec.to_string(), |spec, (dep, shipped)| {
            relax_dependency_bounds(&spec, dep, shipped)
        })
}

#[cfg(test)]
mod relax_bound_tests;

/// Whether `requirement` is a package a spec can name: `%{python_module ...}`
/// around it is allowed, and so is a version constraint after the name. The
/// name must match `^[A-Za-z0-9][A-Za-z0-9._+-]*$`; the constraint may hold only
/// letters, digits, spaces and `. _ + - < > = ! ~ , *`, so no macro, shell
/// or spec character reaches the line.
fn is_plain_build_requirement(requirement: &str) -> bool {
    let inner = requirement
        .strip_prefix("%{python_module ")
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(requirement)
        .trim();
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')))
        .unwrap_or(inner.len());
    let (name, constraint) = inner.split_at(name_end);
    name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && constraint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._+-<>=!~,*".contains(c))
}

/// `spec` with `BuildRequires:  <requirement>` added after the last
/// `BuildRequires:` line, or `None` when it is already there, or the name is
/// not plain (a warning is logged). Used to settle a provider the solver cannot
/// choose between.
pub fn add_build_requires(spec: &str, requirement: &str) -> Option<String> {
    let wanted = requirement.trim();
    if !is_plain_build_requirement(wanted) {
        tracing::warn!("not adding BuildRequires {wanted:?}: not a plain package name");
        return None;
    }
    let has = spec.lines().any(|l| {
        l.trim_start()
            .strip_prefix("BuildRequires:")
            .is_some_and(|v| v.trim() == wanted)
    });
    if has {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("BuildRequires:"))?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at + 1, format!("BuildRequires:  {wanted}"));
    Some(out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod runtime_dependency_tests;

#[cfg(test)]
mod add_build_requires_tests;

#[cfg(test)]
mod conditional_dependency_tests;

#[cfg(test)]
mod hashed_libs_exclude_tests;

#[cfg(test)]
mod redist_requires_tests;

#[cfg(test)]
mod wheel_generator_tests;

#[cfg(test)]
mod license_url_injection_tests;

#[cfg(test)]
mod exclusive_arch_tests;
