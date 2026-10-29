//! The header cleanups and the payload of a wheel: what the spec header and the
//! file list take from the wheel.

/// How `%install` clears one bundled header directory of a prebuilt wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HeaderCleanup<'a> {
    /// The directory holds nothing but headers: `rm -rf` it.
    Dir(&'a str),
    /// The directory also holds files the package needs (a Python package's
    /// `__init__.py`, Cython sources, the `.c` a tool reads at run time): only
    /// its `.h`/`.hpp` files go.
    Headers(&'a str),
}

/// What `%install` removes of a wheel's bundled headers (see
/// [`HeaderCleanup`]); nothing for a wheel whose payload IS headers: when
/// everything outside the header directories is data (no `.py`, no shared
/// library, nothing under `bin/`), the wheel is a header library (NVIDIA's
/// `nvidia-cuda-crt` is only `include/crt/*.h`, `nvidia-cuda-cccl` is CCCL's
/// header tree), and stripping the headers leaves an empty package. Those are
/// split into a `-devel` package by the review (convention 24).
pub(super) fn wheel_header_cleanups(
    layout: &ap_ecosystem::sdist::WheelLayout,
) -> Vec<HeaderCleanup<'_>> {
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
pub(super) fn wheel_payload_probe(layout: &ap_ecosystem::sdist::WheelLayout) -> Option<String> {
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
pub(super) fn wheel_files_entries(
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

/// The top-level directories of `layout`'s payload (dist-info, `.data` and
/// bytecode directories left out).
pub(super) fn payload_tops(layout: &ap_ecosystem::sdist::WheelLayout) -> Vec<&str> {
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
