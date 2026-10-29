use super::*;

/// Which package of the split a file lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Runtime,
    Devel,
}

/// One archive member, mapped to where it is installed.
#[derive(Debug, Clone)]
pub(super) struct Member {
    /// Path inside the install prefix (`lib64/libcudart.so.13`).
    pub(super) dest: String,
    pub(super) role: Role,
}

/// The classified archive.
pub(super) struct Layout {
    pub(super) top: String,
    /// `LICENSE*` files at the archive root (`%license`, not installed).
    pub(super) licenses: Vec<String>,
    pub(super) members: Vec<Member>,
    /// Top-level entries of the archive (without the license files), as
    /// `(source, dest, is_dir)`.
    pub(super) top_entries: Vec<(String, String, bool)>,
}

/// `cuda_cudart` -> `cudart`, `libcublas` -> `cublas`, `cudnn` -> `cudnn`;
/// `_` becomes `-`. The package is `cuda-<short>`.
pub(crate) fn short_name(component_key: &str) -> String {
    let k = component_key
        .strip_prefix("cuda_")
        .or_else(|| component_key.strip_prefix("lib"))
        .unwrap_or(component_key);
    k.replace('_', "-")
}

/// The manifest name, prefixed with NVIDIA unless it already is, as one line
/// with every `%` doubled: it goes into `Summary:` and `%description`, where rpm
/// expands macros.
pub(super) fn human_name(name: &str) -> String {
    let name = spec_text(name);
    if name.starts_with("NVIDIA") {
        name
    } else {
        format!("NVIDIA {name}")
    }
}

/// A name that is safe to put unquoted into a shell command and a macro-free
/// spec line: the archive's directory names come from a member list.
pub(super) fn is_plain_name(name: &str) -> bool {
    // `.` and `..` are made of plain characters but name the directory itself
    // or its parent: `cp -a ..` would copy the unpack directory's parent.
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// `13.4.3` -> `("13", "4")`.
pub(super) fn major_minor(release: &str) -> (String, String) {
    // Digits only: the release reaches `%global` lines and the `Requires` of
    // the filesystem package, so nothing else may pass through it.
    let num = |p: Option<&str>| {
        p.map(|p| p.chars().filter(char::is_ascii_digit).collect::<String>())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "0".to_string())
    };
    let mut it = release.split('.');
    (num(it.next()), num(it.next()))
}

/// The archive's top directory: the first path component every member shares.
/// A leading `./` is ignored, and the archive root itself (`./`) is no member.
/// Members that disagree on the first component have no top directory.
pub(super) fn top_dir(files: &[String]) -> Option<String> {
    let mut top: Option<&str> = None;
    for f in files {
        let path = member_path(f);
        if path.is_empty() {
            continue;
        }
        let first = path.split('/').next().unwrap_or_default();
        match top {
            None => top = Some(first),
            Some(t) if t != first => return None,
            Some(_) => {}
        }
    }
    top.filter(|t| is_plain_name(t)).map(str::to_string)
}

/// The archive ships `lib/`; NVIDIA's own tree (what nvcc, cmake's FindCUDA and
/// every `-L$CUDA_HOME/lib64` expect) calls it `lib64`. Only the first path
/// component is renamed.
pub(super) fn map_dest(rel: &str) -> String {
    match rel.strip_prefix("lib/") {
        Some(rest) => format!("lib64/{rest}"),
        None => rel.to_string(),
    }
}

pub(super) fn is_license_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.starts_with("LICENSE") || u.starts_with("EULA") || u.starts_with("COPYING")
}

/// Top-level directories that hold no run-time file: samples, documentation and
/// helper scripts (`cufile/tools/gds_log_collection.py`). They go with the devel
/// package so the runtime one does not carry their `/usr/bin/perl`, `bash`
/// or `python` requirements.
pub(super) const AUX_DIRS: &[&str] = &[
    "samples",
    "doc",
    "docs",
    "man",
    "tools",
    "extras/CUPTI/samples",
    "extras/CUPTI/doc",
];

pub(super) fn is_aux(dest: &str) -> bool {
    // A top-level README is documentation too.
    (!dest.contains('/') && dest.to_ascii_uppercase().starts_with("README"))
        || AUX_DIRS
            .iter()
            .any(|d| dest.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
}

/// Development files: headers, static libraries, link-time symlinks, build
/// system metadata, the driver stubs, and samples, documentation and helper
/// tools ([`AUX_DIRS`]). Everything else (shared libraries,
/// executables, nvcc's profile and stubs, libdevice bitcode, `extras/` and
/// `nvvm/` content that the tools load while running) is runtime: nvcc stays
/// usable from the main package alone.
pub(super) fn classify(dest: &str, all: &BTreeSet<&str>) -> Role {
    let segs: Vec<&str> = dest.split('/').collect();
    let (dirs, file) = segs.split_at(segs.len() - 1);
    let file = file[0];
    if is_aux(dest) {
        return Role::Devel;
    }
    if dirs.iter().any(|d| {
        matches!(
            *d,
            "include" | "cmake" | "pkgconfig" | "pkg-config" | "stubs"
        )
    }) {
        return Role::Devel;
    }
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if matches!(ext, "a" | "h" | "hpp" | "hh" | "cuh" | "cmake" | "pc") {
        return Role::Devel;
    }
    // `libX.so` is the link-time name only when a versioned `libX.so.N` sits
    // next to it; a lone one is a plugin loaded by dlopen and is runtime.
    if let Some(stem) = file.strip_suffix(".so") {
        let dir = &dest[..dest.len() - file.len()];
        let versioned = format!("{dir}{stem}.so.");
        if all.iter().any(|o| o.starts_with(&versioned)) {
            return Role::Devel;
        }
    }
    Role::Runtime
}

/// A member name without the leading `./` that tar listings commonly carry.
/// Only the prefix is removed: `.` and `..` inside the name stay, and the
/// checks in [`analyse`] refuse them.
pub(super) fn member_path(name: &str) -> &str {
    let mut name = name;
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    name
}

pub(super) fn analyse(files: &[String]) -> Option<Layout> {
    let top = top_dir(files)?;
    let prefix = format!("{top}/");
    let mut licenses = Vec::new();
    let mut raw: Vec<String> = Vec::new();
    let mut top_entries: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for f in files {
        let Some(rel) = member_path(f).strip_prefix(&prefix) else {
            continue;
        };
        let is_dir = rel.ends_with('/');
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let first = rel.split('/').next().unwrap();
        // Copied by `cp -a <first>` in `%install`: never an option or a name
        // the shell would split or expand. Deeper directories are named in the
        // `%install` shell lines too (the `sed` over each `.pc` directory), so
        // every component of every member is held to the same rule.
        // Such a member is left out, not the whole archive: the rest still
        // makes a package, and the member is never copied.
        if !rel.split('/').all(is_plain_name) {
            tracing::warn!("not installing archive member {f:?}: its name is not a plain path");
            continue;
        }
        if !rel.contains('/') && !is_dir && is_license_name(rel) {
            licenses.push(rel.to_string());
            continue;
        }
        let has_children = rel.contains('/');
        let entry_is_dir = has_children || is_dir;
        top_entries.entry(first.to_string()).or_insert_with(|| {
            (
                map_dest(&format!("{first}/"))
                    .trim_end_matches('/')
                    .to_string(),
                entry_is_dir,
            )
        });
        if !is_dir {
            raw.push(map_dest(rel));
        }
    }
    raw.sort();
    raw.dedup();
    licenses.sort();
    licenses.dedup();
    let all: BTreeSet<&str> = raw.iter().map(String::as_str).collect();
    let members = raw
        .iter()
        .map(|d| Member {
            dest: d.clone(),
            role: classify(d, &all),
        })
        .collect();
    Some(Layout {
        top,
        licenses,
        members,
        top_entries: top_entries
            .into_iter()
            .map(|(src, (dest, dir))| (src, dest, dir))
            .collect(),
    })
}

/// Whether an archive with these members makes a package with something in
/// it. An archive with no single top directory, or whose members are all
/// licenses or were all refused, would build an empty package.
pub fn check_layout(files: &[String]) -> Result<(), String> {
    match analyse(files) {
        None => Err("its members have no single top directory with a usable name".into()),
        Some(layout) if layout.members.is_empty() => {
            Err("it has no file to install (only licenses, or every member was refused)".into())
        }
        Some(_) => Ok(()),
    }
}

pub(super) fn has_shared_libs(layout: &Layout) -> bool {
    layout.members.iter().any(|m| {
        m.role == Role::Runtime
            && m.dest.starts_with("lib64/")
            && m.dest
                .rsplit('/')
                .next()
                .is_some_and(|f| f.contains(".so."))
    })
}

pub(super) fn has_stubs(layout: &Layout) -> bool {
    layout.members.iter().any(|m| m.dest.contains("/stubs/"))
}

/// The directories of `%{cuda_prefix}` a package owns: an ancestor directory of
/// any of its files belongs to the runtime package when it holds a runtime
/// file, else to the devel package; the prefix itself is always the runtime's.
pub(super) fn dir_owners(members: &[Member]) -> BTreeMap<String, Role> {
    let mut owners: BTreeMap<String, Role> = BTreeMap::new();
    for m in members {
        let segs: Vec<&str> = m.dest.split('/').collect();
        for n in 1..segs.len() {
            let dir = segs[..n].join("/");
            let slot = owners.entry(dir).or_insert(m.role);
            if m.role == Role::Runtime {
                *slot = Role::Runtime;
            }
        }
    }
    owners
}

/// Whether `dest` is an ELF file as far as the member names tell: a file of
/// `bin/` that is not a profile or stub, or a shared library that is not a
/// shorter name of a longer versioned one (`libX.so`, `libX.so.13` are links
/// to `libX.so.13.4.92`; a lone `stubs/libcuda.so` is a file).
pub(super) fn maybe_elf(dest: &str, all: &BTreeSet<&str>) -> bool {
    let file = dest.rsplit('/').next().unwrap_or(dest);
    if dest.starts_with("bin/") || dest.contains("nvvm/bin/") {
        return !file.ends_with(".profile") && !file.ends_with(".stub");
    }
    if file.ends_with(".so") || file.contains(".so.") {
        let longer = format!("{dest}.");
        return !all.iter().any(|o| o.starts_with(&longer));
    }
    false
}
