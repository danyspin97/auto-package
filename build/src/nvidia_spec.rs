//! Spec generation for NVIDIA's CUDA redistributable components.
//!
//! Each component (`cuda_cudart`, `libcublas`, `cuda_nvcc`, `cudnn`, ...) is one
//! `.tar.xz` per platform on `developer.download.nvidia.com/compute/cuda/redist/`.
//! The archives are closed binaries with this shape (checked on cuda_cudart,
//! cuda_nvtx, cuda_crt, cuda_nvdisasm, cuda_nvcc and cccl, 13.4.x):
//!
//! - one top directory `<key>-linux-<arch>-<version>-archive/`;
//! - `LICENSE` at its root (the only top-level file);
//! - `lib/` (NOT `lib64/`) with versioned `libX.so.M.m.p`, the `libX.so.M` and
//!   `libX.so` symlinks, `.a` statics, `lib/stubs/libcuda.so` (cudart),
//!   `lib/cmake/<pkg>/*.cmake` (cccl);
//! - `include/`, `bin/` (executables, plus `nvcc.profile` and `bin/crt/*.stub`),
//!   `pkg-config/*.pc` whose `libdir` points at `targets/x86_64-linux/lib`, a
//!   directory the archive does not have.
//!
//! Nothing here reads the archive: the generator takes the member list.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::NVIDIA_PROPRIETARY;

/// What [`redist_component_spec`] and [`redist_rpmlintrc`] need to know about
/// one component of one CUDA release.
#[derive(Debug, Clone)]
pub struct RedistSpecInput {
    /// Manifest key: `cuda_cudart`, `libcublas`, `cuda_nvcc`, `cudnn`.
    pub component_key: String,
    /// The manifest's human name ("CUDA Runtime (cudart)").
    pub name: String,
    /// The component's own version (`13.4.92`, not the CUDA release).
    pub version: String,
    /// The CUDA release (`13.4.3`); its `major.minor` names the install prefix.
    pub cuda_release: String,
    /// The manifest's `license` string ("CUDA Toolkit", "CCCL EULA", ...).
    pub license: String,
    /// `cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz`.
    pub archive_filename: String,
    /// Where the archive is downloaded from.
    pub archive_url: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    /// Member paths of the archive, directories (trailing `/`) allowed.
    pub files: Vec<String>,
}

/// Which package of the split a file lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Runtime,
    Devel,
}

/// One archive member, mapped to where it is installed.
#[derive(Debug, Clone)]
struct Member {
    /// Path inside the install prefix (`lib64/libcudart.so.13`).
    dest: String,
    role: Role,
}

/// The classified archive.
struct Layout {
    top: String,
    /// `LICENSE*` files at the archive root (`%license`, not installed).
    licenses: Vec<String>,
    members: Vec<Member>,
    /// Top-level entries of the archive (without the license files), as
    /// `(source, dest, is_dir)`.
    top_entries: Vec<(String, String, bool)>,
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
fn human_name(name: &str) -> String {
    let name = spec_text(name);
    if name.starts_with("NVIDIA") {
        name
    } else {
        format!("NVIDIA {name}")
    }
}

/// `text` for a spec tag or description: one line, `%` doubled.
fn spec_text(text: &str) -> String {
    text.replace('%', "%%").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A name that is safe to put unquoted into a shell command and a macro-free
/// spec line: the archive's directory names come from a member list.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// `13.4.3` -> `("13", "4")`.
fn major_minor(release: &str) -> (String, String) {
    let mut it = release.split('.');
    (
        it.next().unwrap_or("0").to_string(),
        it.next().unwrap_or("0").to_string(),
    )
}

/// The archive's top directory: the first path component every member shares.
fn top_dir(files: &[String]) -> Option<String> {
    files
        .iter()
        .find_map(|f| f.split('/').next().filter(|s| !s.is_empty()))
        .filter(|t| is_plain_name(t))
        .map(str::to_string)
}

/// The archive ships `lib/`; NVIDIA's own tree (what nvcc, cmake's FindCUDA and
/// every `-L$CUDA_HOME/lib64` expect) calls it `lib64`. Only the first path
/// component is renamed.
fn map_dest(rel: &str) -> String {
    match rel.strip_prefix("lib/") {
        Some(rest) => format!("lib64/{rest}"),
        None => rel.to_string(),
    }
}

fn is_license_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.starts_with("LICENSE") || u.starts_with("EULA") || u.starts_with("COPYING")
}

/// Top-level directories that hold no run-time file: samples, documentation and
/// helper scripts (`cufile/tools/gds_log_collection.py`). They go with the devel
/// package so the runtime one does not carry their `/usr/bin/perl`, `bash`
/// or `python` requirements.
const AUX_DIRS: &[&str] = &[
    "samples",
    "doc",
    "docs",
    "man",
    "tools",
    "extras/CUPTI/samples",
    "extras/CUPTI/doc",
];

fn is_aux(dest: &str) -> bool {
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
fn classify(dest: &str, all: &BTreeSet<&str>) -> Role {
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

fn analyse(files: &[String]) -> Option<Layout> {
    let top = top_dir(files)?;
    let prefix = format!("{top}/");
    let mut licenses = Vec::new();
    let mut raw: Vec<String> = Vec::new();
    let mut top_entries: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for f in files {
        let Some(rel) = f.strip_prefix(&prefix) else {
            continue;
        };
        let is_dir = rel.ends_with('/');
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let first = rel.split('/').next().unwrap();
        // Copied by `cp -a <first>` in `%install`: never an option or a name
        // the shell would split or expand.
        if !is_plain_name(first) {
            return None;
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

fn has_shared_libs(layout: &Layout) -> bool {
    layout.members.iter().any(|m| {
        m.role == Role::Runtime
            && m.dest.starts_with("lib64/")
            && m.dest
                .rsplit('/')
                .next()
                .is_some_and(|f| f.contains(".so."))
    })
}

fn has_stubs(layout: &Layout) -> bool {
    layout.members.iter().any(|m| m.dest.contains("/stubs/"))
}

/// The directories of `%{cuda_prefix}` a package owns: an ancestor directory of
/// any of its files belongs to the runtime package when it holds a runtime
/// file, else to the devel package; the prefix itself is always the runtime's.
fn dir_owners(members: &[Member]) -> BTreeMap<String, Role> {
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

/// A `%files` line for a path: `%` doubled, glob characters escaped, a path
/// with whitespace quoted. Archive members never contain either in practice.
fn files_entry(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        match c {
            '%' => out.push_str("%%"),
            '*' | '?' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    if out.contains(char::is_whitespace) {
        format!("\"{out}\"")
    } else {
        out
    }
}

/// `%files` body for one package: `%dir` lines, then every file.
fn files_section(members: &[Member], owners: &BTreeMap<String, Role>, role: Role) -> String {
    let mut out = String::new();
    if role == Role::Runtime {
        out.push_str("%dir %{cuda_prefix}\n");
    }
    for (dir, owner) in owners {
        if *owner == role {
            out.push_str(&format!("%dir %{{cuda_prefix}}/{}\n", files_entry(dir)));
        }
    }
    for m in members.iter().filter(|m| m.role == role) {
        out.push_str(&format!("%{{cuda_prefix}}/{}\n", files_entry(&m.dest)));
    }
    out
}

/// The `/etc/ld.so.conf.d` file name of a release: owned by its
/// [`filesystem_package`] alone.
fn ld_conf_name(release: &str) -> String {
    let (maj, min) = major_minor(release);
    format!("cuda-{maj}-{min}.conf")
}

/// The package that owns the `ld.so.conf.d` file of one CUDA release,
/// `cuda-13-4-filesystem`: every library component of that release `Requires`
/// it, so the file is in one package instead of an identical copy in each.
pub fn filesystem_package(release: &str) -> String {
    let (maj, min) = major_minor(release);
    format!("cuda-{maj}-{min}-filesystem")
}

/// The spec of [`filesystem_package`]: no archive, one text file naming the
/// release's `lib64`. It is `noarch` (the file is the same on every
/// architecture; `lib64` is NVIDIA's name on both).
pub fn redist_filesystem_spec(release: &str, version: &str) -> String {
    let name = filesystem_package(release);
    let (maj, min) = major_minor(release);
    format!(
        "#\n\
         # spec file for package {name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         %global cuda_mm {maj}.{min}\n\
         %global cuda_prefix /usr/local/cuda-%{{cuda_mm}}\n\
         \n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        Library path of the NVIDIA CUDA {maj}.{min} components\n\
         License:        MIT\n\
         URL:            https://developer.nvidia.com/cuda-toolkit\n\
         Source99:       {name}.rpmlintrc\n\
         BuildArch:      noarch\n\
         \n\
         %description\n\
         The ld.so configuration that adds the library directory of the NVIDIA CUDA\n\
         {maj}.{min} tree, %{{cuda_prefix}}/lib64, to the dynamic linker's search path.\n\
         The cuda-* library packages of that release require it so that the file is\n\
         owned once.\n\
         \n\
         %prep\n\
         \n\
         %build\n\
         \n\
         %install\n\
         mkdir -p %{{buildroot}}%{{_sysconfdir}}/ld.so.conf.d\n\
         echo %{{cuda_prefix}}/lib64 > %{{buildroot}}%{{_sysconfdir}}/ld.so.conf.d/{conf}\n\
         \n\
         %files\n\
         %config(noreplace) %{{_sysconfdir}}/ld.so.conf.d/{conf}\n\
         \n\
         %changelog\n",
        conf = ld_conf_name(release),
    )
}

/// The rpmlintrc of [`filesystem_package`]: nothing to filter.
pub fn redist_filesystem_rpmlintrc() -> String {
    "# nothing to filter: the package is one configuration file\n".to_string()
}

/// The components whose headers the headers of `key` include, so that a build
/// against `cuda-<key>-devel` finds them. The manifests carry no dependency
/// information, and the include graph is stable across releases:
/// `cublas_api.h` and `cufft.h` include `cuda_runtime.h` and `driver_types.h`
/// (cudart), `cusolverDn.h` includes `cublas_v2.h` and `cusparse.h`. Header
/// dependencies only: the runtime packages' needs are the sonames' (rpm adds
/// them itself).
fn devel_requires(key: &str) -> &'static [&'static str] {
    match key {
        "libcusolver" => &["cuda_cudart", "libcublas", "libcusparse"],
        "libcublas" | "libcufft" | "libcurand" | "libcusparse" | "libcufile" | "libnvjpeg"
        | "libnpp" | "cuda_cupti" | "cudnn" | "libcusparse_lt" | "libnvshmem" => {
            &["cuda_cudart"]
        }
        _ => &[],
    }
}

/// A complete spec for one redistributable component.
///
/// Layout decisions:
/// - Everything installs under NVIDIA's own tree `/usr/local/cuda-<major>.<minor>`
///   (`bin`, `lib64`, `include`, `nvvm`, ...), not under FHS paths. nvcc finds
///   `ptxas`, `nvlink`, `cicc`, `crt/` and the headers relative to its own
///   location, and `libcudart_static.a`, cccl and the `.pc` files assume the
///   tree, so one `CUDA_HOME` serves every component, toolchain or plain
///   library. The `/usr/local/cuda` symlink belongs to a future meta package.
/// - The archive's `lib/` is installed as `lib64/` (NVIDIA's name for it).
/// - Packages: `cuda-<short>` (shared libraries with soname symlinks, tools in
///   `bin/`, what the tools load while running) and `cuda-<short>-devel`
///   (headers, `.so` link symlinks, `.a`, pkg-config and cmake files, driver
///   stubs). A component with nothing at run time (cccl: headers and cmake
///   files only) is one package `cuda-<short>` that also provides `-devel`.
pub fn redist_component_spec(input: &RedistSpecInput) -> String {
    let short = short_name(&input.component_key);
    let name = format!("cuda-{short}");
    let (maj, min) = major_minor(&input.cuda_release);
    let layout = analyse(&input.files).unwrap_or(Layout {
        top: input
            .archive_filename
            .trim_end_matches(".tar.xz")
            .to_string(),
        licenses: Vec::new(),
        members: Vec::new(),
        top_entries: Vec::new(),
    });
    let has_runtime = layout.members.iter().any(|m| m.role == Role::Runtime);
    // A header-only component is a single package.
    let members: Vec<Member> = layout
        .members
        .iter()
        .map(|m| Member {
            dest: m.dest.clone(),
            role: if has_runtime { m.role } else { Role::Runtime },
        })
        .collect();
    let split = has_runtime && members.iter().any(|m| m.role == Role::Devel);
    let owners = dir_owners(&members);
    let libs = has_shared_libs(&layout);
    let arch = &input.arch;
    let rdma_lib = layout
        .members
        .iter()
        .any(|m| m.dest.rsplit('/').next().is_some_and(|f| f.starts_with("libcufile_rdma.so")));

    let mut s = String::new();
    s.push_str(&format!(
        "#\n\
         # spec file for package {name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n"
    ));
    s.push_str(&format!(
        "# NVIDIA's CUDA {rel} tree: nvcc and its tools locate each other relative to\n\
         # it, so every component installs there and one CUDA_HOME serves them all.\n\
         %global cuda_mm {maj}.{min}\n\
         %global cuda_prefix /usr/local/cuda-%{{cuda_mm}}\n\
         \n\
         # Closed binaries: nothing to strip, no debug information to split, and no\n\
         # post-install tweaking (the brp scripts would rewrite NVIDIA's files).\n\
         %global debug_package %{{nil}}\n\
         %global __strip /bin/true\n\
         %global __os_install_post %{{nil}}\n",
        rel = input.cuda_release
    ));
    if has_stubs(&layout) {
        s.push_str(
            "\n\
             # lib64/stubs/libcuda.so carries SONAME libcuda.so.1, which would make this\n\
             # package Provide the driver's library; only the real driver may.\n\
             %global __provides_exclude_from ^%{cuda_prefix}/lib64/stubs/\n",
        );
    }
    if libs {
        s.push_str(
            "\n\
             # The CUDA libraries load the driver (libcuda.so.1) and NVML\n\
             # (libnvidia-ml.so.1) from the host's NVIDIA driver, which no package of\n\
             # this repository provides.\n\
             %global __requires_exclude ^(libcuda\\.so\\.1|libnvidia-ml\\.so\\.1).*$\n",
        );
    }
    // GPUDirect Storage's RDMA plug-in links the host's InfiniBand stack
    // (libmlx5, librdmacm, libnuma); a plain libcufile user does not need it, so
    // its dependencies are Recommends of the package, not Requires.
    let aux = layout.members.iter().any(|m| is_aux(&m.dest));
    if aux || rdma_lib {
        let mut patterns: Vec<&str> = Vec::new();
        let mut why = String::new();
        if aux {
            patterns
                .push("^%{cuda_prefix}/(samples|tools|doc|docs|man|extras/CUPTI/(samples|doc))/");
            why.push_str(
                "# Sample programs and helper scripts need perl, bash or python, and tools\n\
                 # of the host (libmlx5, ...): those are not requirements of the libraries.\n",
            );
        }
        if rdma_lib {
            patterns.push("^%{cuda_prefix}/lib64/libcufile_rdma\\.so");
            why.push_str(
                "# libcufile_rdma.so links libmlx5, librdmacm and libnuma, which only an\n\
                 # RDMA setup uses: they are Recommends, not Requires, of the package.\n",
            );
        }
        s.push_str(&format!(
            "\n{why}%global __requires_exclude_from {}\n",
            patterns.join("|")
        ));
    }
    s.push_str(&format!(
        "\n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        {human}\n\
         License:        {license}\n\
         URL:            https://developer.nvidia.com/cuda-toolkit\n\
         # NVIDIA's archive, committed next to the spec as downloaded from here.\n\
         Source0:        {url}\n\
         Source99:       {name}.rpmlintrc\n\
         ExclusiveArch:  {arch}\n",
        version = input.version,
        human = human_name(&input.name),
        license = NVIDIA_PROPRIETARY,
        url = input.archive_url,
    ));
    if !split {
        s.push_str("Provides:       %{name}-devel = %{version}-%{release}\n");
    }
    if libs {
        // The ld.so.conf.d file of the release is that package's alone.
        s.push_str(&format!(
            "Requires:       {}\n",
            filesystem_package(&input.cuda_release)
        ));
    }
    if rdma_lib {
        s.push_str(
            "Recommends:     libmlx5-1\nRecommends:     librdmacm1\nRecommends:     libnuma1\n",
        );
    }
    s.push_str(&format!(
        "\n\
         %description\n\
         {human} ({key} {version}) from the NVIDIA CUDA {rel} release\n\
         components, repackaged unchanged from NVIDIA's {lic} licensed archive\n\
         and installed under %{{cuda_prefix}}.\n",
        human = human_name(&input.name),
        key = input.component_key,
        version = input.version,
        rel = input.cuda_release,
        lic = spec_text(&input.license),
    ));
    if split {
        s.push_str(&format!(
            "\n\
             %package devel\n\
             Summary:        Development files for {human}\n\
             Requires:       %{{name}} = %{{version}}-%{{release}}\n\
             {devel_requires}\
             \n\
             %description devel\n\
             Headers, link-time links, static libraries and build-system files of\n\
             {human} ({key} {version}), installed under %{{cuda_prefix}}.\n",
            human = human_name(&input.name),
            key = input.component_key,
            version = input.version,
            devel_requires = devel_requires(&input.component_key)
                .iter()
                .map(|k| format!("Requires:       cuda-{}-devel\n", short_name(k)))
                .collect::<String>(),
        ));
    }

    s.push_str(&format!("\n%prep\n%setup -q -n {}\n", layout.top));
    s.push_str("\n%build\n# Prebuilt binaries: nothing to build.\n");
    s.push_str("\n%install\nmkdir -p %{buildroot}%{cuda_prefix}\n");
    for (src, dest, is_dir) in &layout.top_entries {
        if *is_dir {
            s.push_str(&format!(
                "mkdir -p %{{buildroot}}%{{cuda_prefix}}/{dest}\n\
                 cp -a {src}/. %{{buildroot}}%{{cuda_prefix}}/{dest}/\n"
            ));
        } else {
            s.push_str(&format!("cp -a {src} %{{buildroot}}%{{cuda_prefix}}/\n"));
        }
    }
    let pc_dirs: BTreeSet<&str> = layout
        .members
        .iter()
        .filter(|m| m.dest.ends_with(".pc"))
        .filter_map(|m| m.dest.rsplit_once('/').map(|(d, _)| d))
        .collect();
    for dir in pc_dirs {
        s.push_str(&format!(
            "# The .pc files point at targets/<arch>-linux/{{lib,include}}, which this\n\
             # tree does not have: they must name the directories that are installed.\n\
             sed -i \\\n\
             \x20 -e 's#^cudaroot=.*#cudaroot=%{{cuda_prefix}}#' \\\n\
             \x20 -e 's#^libdir=.*#libdir=${{cudaroot}}/lib64#' \\\n\
             \x20 -e 's#^includedir=.*#includedir=${{cudaroot}}/include#' \\\n\
             \x20 %{{buildroot}}%{{cuda_prefix}}/{dir}/*.pc\n"
        ));
    }
    if libs {
        s.push_str("\n%ldconfig_scriptlets\n");
    }

    let lic = |s: &mut String| {
        for l in &layout.licenses {
            s.push_str(&format!("%license {}\n", files_entry(l)));
        }
    };
    s.push_str("\n%files\n");
    lic(&mut s);
    s.push_str(&files_section(&members, &owners, Role::Runtime));
    if split {
        s.push_str("\n%files devel\n");
        lic(&mut s);
        s.push_str(&files_section(&members, &owners, Role::Devel));
    }
    s.push_str("\n%changelog\n");
    s
}

/// Whether `dest` is an ELF file as far as the member names tell: a file of
/// `bin/` that is not a profile or stub, or a shared library that is not a
/// shorter name of a longer versioned one (`libX.so`, `libX.so.13` are links
/// to `libX.so.13.4.92`; a lone `stubs/libcuda.so` is a file).
fn maybe_elf(dest: &str, all: &BTreeSet<&str>) -> bool {
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

/// The rpmlintrc matching [`redist_component_spec`]: only the filters the
/// layout really triggers (`unused-rpmlintrc-filter` is an error of its own).
/// Checked with rpmlint 2.10 on the built cuda_cudart, cuda_nvtx, cuda_crt,
/// cuda_nvdisasm and cccl packages.
pub fn redist_rpmlintrc(input: &RedistSpecInput) -> String {
    let mut out = String::new();
    // Every file is under /usr/local by design, see `redist_component_spec`.
    out.push_str(
        "# NVIDIA's CUDA tree lives in /usr/local/cuda-<major>.<minor>: nvcc finds\n\
         # its tools and headers relative to that tree.\n\
         addFilter(\"filelist-forbidden-fhs23\")\n\
         addFilter(\"dir-or-file-outside-snapshot\")\n",
    );
    let Some(layout) = analyse(&input.files) else {
        return out;
    };
    if has_shared_libs(&layout) {
        out.push_str(
            "\n\
             # The libraries are NVIDIA's prebuilt binaries, linked without a hash\n\
             # section; they cannot be relinked here.\n\
             addFilter(\"missing-hash-section\")\n",
        );
    }
    if layout.members.iter().any(|m| m.dest.ends_with(".a")) {
        out.push_str(
            "\n\
             # The static libraries are NVIDIA's prebuilt archives; they carry no\n\
             # debug information.\n\
             addFilter(\"static-library-without-debuginfo\")\n",
        );
    }
    // A package of the split without a single ELF file (the headers of a
    // component whose libraries live in the other package): arch-specific
    // because the source archive is, but rpmlint wants a binary in it.
    let has_runtime = layout.members.iter().any(|m| m.role == Role::Runtime);
    let all: BTreeSet<&str> = layout.members.iter().map(|m| m.dest.as_str()).collect();
    let elf = |role: Option<Role>| {
        layout
            .members
            .iter()
            .filter(|m| role.is_none_or(|r| m.role == r))
            .any(|m| maybe_elf(&m.dest, &all))
    };
    let lacks_binary = if has_runtime {
        let has_devel = layout.members.iter().any(|m| m.role == Role::Devel);
        !elf(Some(Role::Runtime)) || (has_devel && !elf(Some(Role::Devel)))
    } else {
        !elf(None)
    };
    if lacks_binary {
        out.push_str(
            "\n\
             # Headers and build-system files only: the package is arch-specific\n\
             # because NVIDIA's archive is per platform.\n\
             addFilter(\"no-binary\")\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::todo_placeholders;

    const CUDART_TOP: &str = "cuda_cudart-linux-x86_64-13.4.92-archive";
    // Real member list of cuda_cudart 13.4.92 with the 100-odd headers cut to
    // a few that cover the sub-directories.
    const CUDART: &str = "\
lib/
lib/stubs/
lib/stubs/libcuda.so
lib/libcudart.so.13.4.92
lib/libcudart.so.13
lib/libcudart.so
lib/libcudart_static.a
lib/libcudadevrt.a
LICENSE
pkg-config/
pkg-config/cuda-13.4.pc
pkg-config/cudart-13.4.pc
include/
include/cuda_runtime.h
include/cuda_fp16.h
include/cooperative_groups.h
include/cooperative_groups/details/driver_abi.h
include/cooperative_groups/reduce.h
";
    const CCCL: &str = "\
LICENSE
lib/cmake/cccl/cccl-config.cmake
lib/cmake/cub/cub-config.cmake
lib/cmake/thrust/FindTBB.cmake
lib/cmake/thrust/README.md
include/cccl/cub/agent/agent_for.cuh
include/cccl/cub/block/block_load.cuh
include/cccl/thrust/sort.h
include/cccl/cuda/std/algorithm.sort_heap.h
";
    const NVCC: &str = "\
LICENSE
include/fatbinary_section.h
bin/crt/link.stub
bin/crt/prelink.stub
bin/bin2c
bin/cudafe++
bin/fatbinary
bin/ptxas
bin/nvcc.profile
bin/nvcc
bin/nvlink
bin/__nvcc_device_query
";

    fn input(key: &str, version: &str, list: &str) -> RedistSpecInput {
        let top = format!("{key}-linux-x86_64-{version}-archive");
        RedistSpecInput {
            component_key: key.into(),
            name: format!("Component {key}"),
            version: version.into(),
            cuda_release: "13.4.3".into(),
            license: "CUDA Toolkit".into(),
            archive_filename: format!("{top}.tar.xz"),
            archive_url: format!(
                "https://developer.download.nvidia.com/compute/cuda/redist/{key}/linux-x86_64/{top}.tar.xz"
            ),
            arch: "x86_64".into(),
            files: list
                .lines()
                .map(|l| format!("{top}/{l}"))
                .chain(std::iter::once(format!("{top}/")))
                .collect(),
        }
    }

    fn cudart() -> RedistSpecInput {
        let i = input("cuda_cudart", "13.4.92", CUDART);
        assert!(i.files[0].starts_with(CUDART_TOP));
        i
    }

    /// The lines of a `%files` section of `spec` (header line excluded).
    fn section<'a>(spec: &'a str, header: &str) -> Vec<&'a str> {
        let mut lines = spec.lines();
        lines.find(|l| *l == header).expect(header);
        lines
            .take_while(|l| {
                !l.starts_with('%')
                    || l.starts_with("%dir")
                    || l.starts_with("%{")
                    || l.starts_with("%license")
                    || l.starts_with("%config")
            })
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    fn names_and_header() {
        assert_eq!(short_name("cuda_cudart"), "cudart");
        assert_eq!(short_name("libcublas"), "cublas");
        assert_eq!(short_name("cudnn"), "cudnn");
        assert_eq!(short_name("cuda_nvrtc_dev"), "nvrtc-dev");
        let spec = redist_component_spec(&cudart());
        assert!(spec.contains("Name:           cuda-cudart\n"), "{spec}");
        assert!(spec.contains("License:        SUSE-NonFree\n"), "{spec}");
        assert!(spec.contains("ExclusiveArch:  x86_64\n"), "{spec}");
        assert!(spec.contains("%global cuda_mm 13.4\n"), "{spec}");
        assert!(
            spec.contains(&format!("%setup -q -n {CUDART_TOP}\n")),
            "{spec}"
        );
        assert!(
            spec.contains("Source0:        https://developer.download.nvidia.com/"),
            "{spec}"
        );
        assert!(spec.contains("%package devel\n"), "{spec}");
        assert!(
            spec.contains("Requires:       %{name} = %{version}-%{release}\n"),
            "{spec}"
        );
        assert!(spec.contains("%global __strip /bin/true\n"), "{spec}");
        assert!(spec.contains("%global debug_package %{nil}\n"), "{spec}");
    }

    #[test]
    fn runtime_devel_partition_on_cudart() {
        let spec = redist_component_spec(&cudart());
        let rt = section(&spec, "%files");
        let dv = section(&spec, "%files devel");
        let p = "%{cuda_prefix}/";
        for f in ["lib64/libcudart.so.13.4.92", "lib64/libcudart.so.13"] {
            assert!(
                rt.contains(&format!("{p}{f}").as_str()),
                "{f} runtime:\n{spec}"
            );
        }
        for f in [
            "lib64/libcudart.so",
            "lib64/libcudart_static.a",
            "lib64/libcudadevrt.a",
            "lib64/stubs/libcuda.so",
            "include/cuda_runtime.h",
            "include/cooperative_groups/details/driver_abi.h",
            "pkg-config/cudart-13.4.pc",
        ] {
            assert!(
                dv.contains(&format!("{p}{f}").as_str()),
                "{f} devel:\n{spec}"
            );
            assert!(!rt.contains(&format!("{p}{f}").as_str()), "{f} not runtime");
        }
        // Every non-directory member is listed exactly once across both.
        let all: Vec<&str> = rt
            .iter()
            .chain(dv.iter())
            .copied()
            .filter(|l| l.starts_with("%{cuda_prefix}/"))
            .collect();
        assert_eq!(all.len(), 13, "{all:?}");
        let uniq: BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(uniq.len(), all.len());
        // The licence is a %license of both, not an installed file.
        assert!(rt.contains(&"%license LICENSE") && dv.contains(&"%license LICENSE"));
    }

    #[test]
    fn files_are_exact_and_dirs_have_one_owner() {
        let spec = redist_component_spec(&cudart());
        let files = spec.split("%files\n").nth(1).unwrap();
        assert!(!files.contains('*'), "no globs in %files:\n{files}");
        let rt = section(&spec, "%files");
        let dv = section(&spec, "%files devel");
        assert!(rt.contains(&"%dir %{cuda_prefix}"));
        assert!(rt.contains(&"%dir %{cuda_prefix}/lib64"));
        // lib64/stubs and include hold only devel files: devel owns them.
        assert!(dv.contains(&"%dir %{cuda_prefix}/lib64/stubs"));
        assert!(dv.contains(&"%dir %{cuda_prefix}/include/cooperative_groups/details"));
        assert!(!rt.contains(&"%dir %{cuda_prefix}/include"));
        let dirs = |v: &[&str]| {
            v.iter()
                .filter(|l| l.starts_with("%dir "))
                .map(|l| l.to_string())
                .collect::<BTreeSet<_>>()
        };
        assert!(dirs(&rt).is_disjoint(&dirs(&dv)));
    }

    #[test]
    fn lib_is_installed_as_lib64_and_pc_files_are_fixed() {
        let spec = redist_component_spec(&cudart());
        assert!(
            spec.contains("cp -a lib/. %{buildroot}%{cuda_prefix}/lib64/\n"),
            "{spec}"
        );
        assert!(
            spec.contains("cp -a include/. %{buildroot}%{cuda_prefix}/include/\n"),
            "{spec}"
        );
        assert!(
            spec.contains("s#^libdir=.*#libdir=${cudaroot}/lib64#"),
            "{spec}"
        );
        assert!(spec.contains("%{cuda_prefix}/pkg-config/*.pc"), "{spec}");
        assert!(!spec.contains("cp -a LICENSE"), "{spec}");
    }

    #[test]
    fn library_component_gets_ldconfig_and_driver_excludes() {
        let spec = redist_component_spec(&cudart());
        assert!(spec.contains("\n%ldconfig_scriptlets\n"), "{spec}");
        // The conf file is the filesystem package's: required, never shipped.
        assert!(spec.contains("\nRequires:       cuda-13-4-filesystem\n"), "{spec}");
        assert!(!spec.contains("ld.so.conf.d"), "{spec}");
        assert!(
            spec.contains("%global __requires_exclude ^(libcuda\\.so\\.1|"),
            "{spec}"
        );
        assert!(
            spec.contains("%global __provides_exclude_from ^%{cuda_prefix}/lib64/stubs/"),
            "{spec}"
        );
        // /usr/local/cuda is a meta package's.
        assert!(!spec.contains("ln -s"), "{spec}");
    }

    #[test]
    fn manifest_text_and_archive_names_cannot_inject_macros_or_commands() {
        let mut i = cudart();
        i.name = "cuBLAS 100% %{expand:x}".into();
        i.license = "EULA %{_bindir}".into();
        let spec = redist_component_spec(&i);
        assert!(spec.contains("Summary:        NVIDIA cuBLAS 100%% %%{expand:x}\n"), "{spec}");
        assert!(spec.contains("EULA %%{_bindir} licensed archive"), "{spec}");
        // A member name the shell would act on makes the archive unusable.
        let files: Vec<String> = ["top/lib/libx.so.1", "top/--help/y", "top/LICENSE"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(analyse(&files).is_none());
        let files: Vec<String> = ["to p/lib/libx.so.1"].iter().map(|s| s.to_string()).collect();
        assert!(analyse(&files).is_none());
        let files: Vec<String> = ["top/lib/libx.so.1".to_string()].to_vec();
        assert!(analyse(&files).is_some());
        // No members at all is not a panic: an empty package.
        let mut empty = cudart();
        empty.files = Vec::new();
        let spec = redist_component_spec(&empty);
        assert!(spec.contains("%files\n"), "{spec}");
        assert!(!redist_rpmlintrc(&empty).is_empty());
    }

    #[test]
    fn the_filesystem_package_owns_the_one_conf_file() {
        let spec = redist_filesystem_spec("13.4.3", "13.4.3");
        assert!(spec.contains("Name:           cuda-13-4-filesystem\n"), "{spec}");
        assert!(spec.contains("BuildArch:      noarch\n"), "{spec}");
        assert!(spec.contains("Version:        13.4.3\n"), "{spec}");
        assert!(!spec.contains("Source0") && !spec.contains("%setup"), "{spec}");
        assert!(
            spec.contains("echo %{cuda_prefix}/lib64 > %{buildroot}%{_sysconfdir}/ld.so.conf.d/cuda-13-4.conf\n"),
            "{spec}"
        );
        assert_eq!(
            section(&spec, "%files"),
            vec!["%config(noreplace) %{_sysconfdir}/ld.so.conf.d/cuda-13-4.conf"]
        );
        assert!(!spec.contains("TODO"), "{spec}");
        // The package a library component requires is the one named here.
        assert_eq!(filesystem_package("13.0"), "cuda-13-0-filesystem");
        assert!(redist_filesystem_rpmlintrc().lines().all(|l| l.starts_with('#')));
    }

    #[test]
    fn header_only_component_is_one_package_without_ldconfig() {
        let spec = redist_component_spec(&input("cccl", "13.3.4.3.1", CCCL));
        assert!(spec.contains("Name:           cuda-cccl\n"), "{spec}");
        assert!(!spec.contains("ldconfig"), "{spec}");
        assert!(!spec.contains("ld.so.conf.d"), "{spec}");
        assert!(!spec.contains("__requires_exclude"), "{spec}");
        assert!(!spec.contains("__provides_exclude_from"), "{spec}");
        assert!(!spec.contains("%package devel"), "{spec}");
        assert!(
            spec.contains("Provides:       %{name}-devel = %{version}-%{release}\n"),
            "{spec}"
        );
        let rt = section(&spec, "%files");
        assert!(
            rt.contains(&"%{cuda_prefix}/include/cccl/cub/agent/agent_for.cuh"),
            "{spec}"
        );
        assert!(
            rt.contains(&"%{cuda_prefix}/lib64/cmake/cccl/cccl-config.cmake"),
            "{spec}"
        );
        assert!(
            rt.contains(&"%dir %{cuda_prefix}/lib64/cmake/thrust"),
            "{spec}"
        );
        assert!(
            rt.contains(&"%{cuda_prefix}/lib64/cmake/thrust/README.md"),
            "{spec}"
        );
        let rc = redist_rpmlintrc(&input("cccl", "13.3.4.3.1", CCCL));
        assert!(!rc.contains("missing-hash-section"), "{rc}");
    }

    #[test]
    fn toolchain_component_keeps_tools_in_the_runtime_package() {
        let spec = redist_component_spec(&input("cuda_nvcc", "13.4.92", NVCC));
        assert!(spec.contains("Name:           cuda-nvcc\n"), "{spec}");
        let rt = section(&spec, "%files");
        let dv = section(&spec, "%files devel");
        for f in [
            "bin/nvcc",
            "bin/ptxas",
            "bin/nvcc.profile",
            "bin/crt/link.stub",
            "bin/nvlink",
        ] {
            assert!(
                rt.contains(&format!("%{{cuda_prefix}}/{f}").as_str()),
                "{f}:\n{spec}"
            );
        }
        assert_eq!(
            dv,
            [
                "%license LICENSE",
                "%dir %{cuda_prefix}/include",
                "%{cuda_prefix}/include/fatbinary_section.h"
            ],
            "{spec}"
        );
        assert!(spec.contains("%dir %{cuda_prefix}/bin/crt\n"), "{spec}");
        assert!(!spec.contains("ldconfig"), "{spec}");
        // A libdevice/nvvm tree is runtime too: nvcc loads it while compiling.
        let nvvm = "LICENSE\nnvvm/bin/cicc\nnvvm/libdevice/libdevice.10.bc\nnvvm/include/nvvm.h\nnvvm/lib64/libnvvm.so.4.0.0\nnvvm/lib64/libnvvm.so.4\nnvvm/lib64/libnvvm.so\n";
        let spec = redist_component_spec(&input("cuda_nvvm", "13.4.93", nvvm));
        let rt = section(&spec, "%files");
        let dv = section(&spec, "%files devel");
        assert!(
            rt.contains(&"%{cuda_prefix}/nvvm/libdevice/libdevice.10.bc"),
            "{spec}"
        );
        assert!(rt.contains(&"%{cuda_prefix}/nvvm/bin/cicc"), "{spec}");
        assert!(
            rt.contains(&"%{cuda_prefix}/nvvm/lib64/libnvvm.so.4"),
            "{spec}"
        );
        assert!(
            dv.contains(&"%{cuda_prefix}/nvvm/lib64/libnvvm.so"),
            "{spec}"
        );
        assert!(dv.contains(&"%{cuda_prefix}/nvvm/include/nvvm.h"), "{spec}");
        // A lone unversioned .so is a plugin, not a link symlink.
        let plug = "LICENSE\nlib/libplugin.so\n";
        let spec = redist_component_spec(&input("cuda_plug", "1.0", plug));
        assert!(!spec.contains("%package devel"), "{spec}");
    }

    #[test]
    fn no_placeholders_in_any_output() {
        for i in [
            cudart(),
            input("cccl", "13.3.4.3.1", CCCL),
            input("cuda_nvcc", "13.4.92", NVCC),
        ] {
            let spec = redist_component_spec(&i);
            assert!(todo_placeholders(&spec).is_empty(), "{spec}");
            assert!(!spec.contains("TODO") && !spec.contains("FIXME"));
            assert!(!redist_rpmlintrc(&i).contains("TODO"));
        }
    }

    #[test]
    fn rpmlintrc_only_filters_what_is_present() {
        // cudart: shared libraries and .a, ELF in both packages (stubs/libcuda.so).
        let rc = redist_rpmlintrc(&cudart());
        assert!(
            rc.contains("addFilter(\"filelist-forbidden-fhs23\")"),
            "{rc}"
        );
        assert!(rc.contains("addFilter(\"missing-hash-section\")"), "{rc}");
        assert!(
            rc.contains("addFilter(\"static-library-without-debuginfo\")"),
            "{rc}"
        );
        assert!(!rc.contains("no-binary"), "{rc}");
        assert!(!rc.contains("library-without-ldconfig"), "{rc}");
        // nvcc: tools only, no shared library, no static library; its devel
        // package is one header.
        let rc = redist_rpmlintrc(&input("cuda_nvcc", "13.4.92", NVCC));
        assert!(!rc.contains("missing-hash-section"), "{rc}");
        assert!(!rc.contains("static-library"), "{rc}");
        assert!(rc.contains("no-binary"), "{rc}");
        // cccl: one package of headers.
        let rc = redist_rpmlintrc(&input("cccl", "13.3.4.3.1", CCCL));
        assert!(
            rc.contains("no-binary") && !rc.contains("missing-hash-section"),
            "{rc}"
        );
        // A tool-only component has a binary and nothing else to filter.
        let rc = redist_rpmlintrc(&input(
            "cuda_nvdisasm",
            "13.4.92",
            "LICENSE\nbin/nvdisasm\n",
        ));
        assert!(
            !rc.contains("no-binary") && !rc.contains("missing-hash-section"),
            "{rc}"
        );
    }

    const CUFILE: &str = "\
LICENSE
README
etc/cufile.json
include/cufile.h
lib/libcufile.so.0
lib/libcufile.so.1.19.1
lib/libcufile.so
lib/libcufile_rdma.so.1
man/man3/cufile.h.3
pkg-config/cufile-13.4.pc
tools/README
tools/gds_log_collection.py
samples/cufile_sample_001.cc
";

    #[test]
    fn samples_docs_man_and_tools_go_to_devel_with_no_interpreter_requires() {
        let spec = redist_component_spec(&input("libcufile", "1.19.1.55", CUFILE));
        let rt = section(&spec, "%files");
        let dv = section(&spec, "%files devel");
        let p = "%{cuda_prefix}/";
        for f in [
            "README",
            "man/man3/cufile.h.3",
            "tools/gds_log_collection.py",
            "tools/README",
            "samples/cufile_sample_001.cc",
        ] {
            let path = format!("{p}{f}");
            assert!(dv.contains(&path.as_str()), "{f} in devel: {dv:?}");
            assert!(!rt.contains(&path.as_str()), "{f} in runtime: {rt:?}");
        }
        for f in ["etc/cufile.json", "lib64/libcufile.so.0", "lib64/libcufile_rdma.so.1"] {
            let path = format!("{p}{f}");
            assert!(rt.contains(&path.as_str()), "{f}: {rt:?}");
        }
        assert!(
            spec.contains("%global __requires_exclude_from ^%{cuda_prefix}/(samples|tools|"),
            "{spec}"
        );
        // The RDMA plug-in's host libraries are Recommends, not Requires.
        assert!(spec.contains("|^%{cuda_prefix}/lib64/libcufile_rdma\\.so\n"), "{spec}");
        for r in ["libmlx5-1", "librdmacm1", "libnuma1"] {
            assert!(spec.contains(&format!("Recommends:     {r}\n")), "{spec}");
        }
        // A component without such files has no exclude.
        let plain = redist_component_spec(&cudart());
        assert!(!plain.contains("__requires_exclude_from"));
        assert!(!plain.contains("Recommends:"));
    }

    #[test]
    fn devel_packages_require_the_devel_of_the_headers_they_include() {
        let files = "LICENSE\ninclude/cusolverDn.h\nlib/libcusolver.so.12\nlib/libcusolver.so\n";
        let spec = redist_component_spec(&input("libcusolver", "12.1.0.1", files));
        let devel = spec
            .split("%package devel")
            .nth(1)
            .unwrap()
            .split("%description devel")
            .next()
            .unwrap();
        for dep in ["cuda-cudart-devel", "cuda-cublas-devel", "cuda-cusparse-devel"] {
            assert!(devel.contains(&format!("Requires:       {dep}\n")), "{devel}");
        }
        // cudart includes nothing of another component.
        let spec = redist_component_spec(&cudart());
        let devel = spec
            .split("%package devel")
            .nth(1)
            .unwrap()
            .split("%description devel")
            .next()
            .unwrap();
        assert_eq!(devel.matches("Requires:").count(), 1, "{devel}");
        assert_eq!(devel_requires("libnvshmem"), ["cuda_cudart"]);
    }

    #[test]
    fn rpmlintrc_filters_the_snapshot_check() {
        assert!(
            redist_rpmlintrc(&cudart()).contains("addFilter(\"dir-or-file-outside-snapshot\")")
        );
    }
}
