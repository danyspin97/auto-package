use super::*;

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

/// `text` for a spec tag or description: one line, `%` doubled.
pub(super) fn spec_text(text: &str) -> String {
    text.replace('%', "%%")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` as an RPM `Version:`: only letters, digits and `. _ + ~`, the
/// characters a version may carry. A `-` (a release suffix such as
/// `13.4.92-1`) becomes `.`, so the parts stay apart. Anything else is removed,
/// so a newline or a `%` cannot open a spec line or a macro; an empty result
/// becomes `0`.
pub(super) fn version_text(text: &str) -> String {
    let v: String = text
        .chars()
        .filter_map(|c| match c {
            '-' => Some('.'),
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~') => Some(c),
            _ => None,
        })
        .collect();
    if v.is_empty() { "0".to_string() } else { v }
}

/// A `%files` line for a path: `%` doubled, glob characters escaped, a path
/// with whitespace or a double quote quoted, the quote itself escaped inside.
/// Archive members never contain either in practice; `analyse` refuses them.
pub(super) fn files_entry(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        match c {
            '%' => out.push_str("%%"),
            '*' | '?' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    if path.contains('"') || out.contains(char::is_whitespace) {
        format!("\"{out}\"")
    } else {
        out
    }
}

/// `%files` body for one package: `%dir` lines, then every file.
pub(super) fn files_section(
    members: &[Member],
    owners: &BTreeMap<String, Role>,
    role: Role,
) -> String {
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
pub(super) fn ld_conf_name(release: &str) -> String {
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
pub(super) fn devel_requires(key: &str) -> &'static [&'static str] {
    match key {
        "libcusolver" => &["cuda_cudart", "libcublas", "libcusparse"],
        "libcublas" | "libcufft" | "libcurand" | "libcusparse" | "libcufile" | "libnvjpeg"
        | "libnpp" | "cuda_cupti" | "cudnn" | "libcusparse_lt" | "libnvshmem" => &["cuda_cudart"],
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
    // `Name:` is written verbatim and the key comes from a manifest: a key that
    // is not a plain name gets a fixed one, not a line break or a macro.
    let short = short_name(&input.component_key);
    let short = if is_plain_name(&format!("cuda-{short}")) {
        short
    } else {
        "component".to_string()
    };
    let name = format!("cuda-{short}");
    let version = version_text(&input.version);
    let (maj, min) = major_minor(&input.cuda_release);
    let layout = analyse(&input.files).unwrap_or(Layout {
        // `%setup -n` names this directory: only a plain name may be written.
        top: Some(input.archive_filename.trim_end_matches(".tar.xz"))
            .filter(|t| is_plain_name(t))
            .unwrap_or("archive")
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
    let rdma_lib = layout.members.iter().any(|m| {
        m.dest
            .rsplit('/')
            .next()
            .is_some_and(|f| f.starts_with("libcufile_rdma.so"))
    });

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
        rel = spec_text(&input.cuda_release)
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
             # this repository provides (the one driver-library rule of depclass).\n",
        );
        s.push_str(&format!(
            "%global __requires_exclude {}\n",
            crate::depclass::driver_requires_exclude()
        ));
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
        version = version,
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
        version = version,
        rel = spec_text(&input.cuda_release),
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
            version = version,
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
