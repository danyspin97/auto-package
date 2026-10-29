use super::*;

pub(super) const CUDART_TOP: &str = "cuda_cudart-linux-x86_64-13.4.92-archive";

// Real member list of cuda_cudart 13.4.92 with the 100-odd headers cut to
// a few that cover the sub-directories.
pub(super) const CUDART: &str = "\
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

pub(super) const CCCL: &str = "\
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

pub(super) const NVCC: &str = "\
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

pub(super) fn input(key: &str, version: &str, list: &str) -> RedistSpecInput {
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

pub(super) fn cudart() -> RedistSpecInput {
    let i = input("cuda_cudart", "13.4.92", CUDART);
    assert!(i.files[0].starts_with(CUDART_TOP));
    i
}

/// The lines of a `%files` section of `spec` (header line excluded).
pub(super) fn section<'a>(spec: &'a str, header: &str) -> Vec<&'a str> {
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
pub(super) fn names_and_header() {
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
pub(super) fn runtime_devel_partition_on_cudart() {
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
pub(super) fn files_are_exact_and_dirs_have_one_owner() {
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
pub(super) fn lib_is_installed_as_lib64_and_pc_files_are_fixed() {
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
pub(super) fn library_component_gets_ldconfig_and_driver_excludes() {
    let spec = redist_component_spec(&cudart());
    assert!(spec.contains("\n%ldconfig_scriptlets\n"), "{spec}");
    // The conf file is the filesystem package's: required, never shipped.
    assert!(
        spec.contains("\nRequires:       cuda-13-4-filesystem\n"),
        "{spec}"
    );
    assert!(!spec.contains("ld.so.conf.d"), "{spec}");
    assert!(
        spec.contains(&format!(
            "%global __requires_exclude {}\n",
            crate::depclass::driver_requires_exclude()
        )),
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
pub(super) fn manifest_text_and_archive_names_cannot_inject_macros_or_commands() {
    let mut i = cudart();
    i.name = "cuBLAS 100% %{expand:x}".into();
    i.license = "EULA %{_bindir}".into();
    let spec = redist_component_spec(&i);
    assert!(
        spec.contains("Summary:        NVIDIA cuBLAS 100%% %%{expand:x}\n"),
        "{spec}"
    );
    assert!(spec.contains("EULA %%{_bindir} licensed archive"), "{spec}");
    // A member name the shell would act on is left out; the rest stays.
    let files: Vec<String> = ["top/lib/libx.so.1", "top/--help/y", "top/LICENSE"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let layout = analyse(&files).unwrap();
    assert!(layout.members.iter().all(|m| !m.dest.contains("--help")));
    assert_eq!(layout.members.len(), 1);
    let files: Vec<String> = ["to p/lib/libx.so.1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
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
pub(super) fn release_and_archive_names_cannot_inject_spec_lines_or_macros() {
    let mut i = cudart();
    i.cuda_release = "13.4%{lua:os.execute(\"id\")}\nRequires: x".into();
    i.archive_filename = "a%{lua:x}.tar.xz".into();
    // Two top directories: no layout, so the fallback names the setup directory.
    i.files = vec!["top/lib/a b$(x)/y.so".into(), "other/lib/y.so".into()];
    let spec = redist_component_spec(&i);
    // Every `%` left is half of a `%%`: no bare macro survives.
    assert!(!spec.replace("%%", "").contains("%{lua"), "{spec}");
    assert!(!spec.contains("\nRequires: x"), "{spec}");
    assert!(!spec.contains("$(x)"), "{spec}");
    assert!(spec.contains("%setup -q -n archive\n"), "{spec}");
}

/// `Version:` and `Name:` take manifest text: a newline or a macro in
/// either cannot open a line of its own or expand.
#[test]
pub(super) fn version_and_name_cannot_inject_spec_lines_or_macros() {
    let mut i = cudart();
    i.version = "13.4.92\nRequires: pwned%{lua:x}".into();
    i.component_key = "cuda_x\nRequires: pwned".into();
    let spec = redist_component_spec(&i);
    // Tags live in the preamble; the description body is free text.
    let preamble = spec.split("%description").next().unwrap_or_default();
    assert!(
        !preamble.lines().any(|l| l.starts_with("Requires: pwned")),
        "{spec}"
    );
    assert!(!spec.contains("%{lua"), "{spec}");
    assert_eq!(
        spec.lines().filter(|l| l.starts_with("Version:")).count(),
        1,
        "{spec}"
    );
    assert!(spec.contains("Name:           cuda-component\n"), "{spec}");
    // A clean version is written as it is.
    let clean = redist_component_spec(&cudart());
    assert!(clean.contains("Version:        13.4.92\n"), "{clean}");
}

/// A release suffix keeps its digits apart: `13.4.92-1` is `13.4.92.1`,
/// not `13.4.921`. The other characters are still removed.
#[test]
pub(super) fn version_text_turns_a_dash_into_a_dot() {
    assert_eq!(version_text("13.4.92-1"), "13.4.92.1");
    assert_eq!(version_text("13.4.92~rc1-2"), "13.4.92~rc1.2");
    assert_eq!(
        version_text("13.4.92\nRequires: x%{lua:y}"),
        "13.4.92Requiresxluay"
    );
    assert_eq!(version_text("%{x}"), "x");
    assert_eq!(version_text("-"), ".");
    assert_eq!(version_text("%%"), "0");
}

#[test]
pub(super) fn fallback_setup_directory_must_be_plain() {
    let mut i = cudart();
    i.archive_filename = "x y.tar.xz".into();
    i.files = vec!["top/lib/a b$(x)/y.so".into(), "other/lib/y.so".into()];
    let spec = redist_component_spec(&i);
    assert!(spec.contains("%setup -q -n archive\n"), "{spec}");
    assert!(!spec.contains("x y"), "{spec}");
}

#[test]
pub(super) fn dot_and_dotdot_names_are_never_plain() {
    assert!(!is_plain_name("."));
    assert!(!is_plain_name(".."));
    assert!(!is_plain_name(""));
    assert!(!is_plain_name("./x"));
    assert!(is_plain_name("..x"));
    assert!(is_plain_name(".hidden"));
    assert!(is_plain_name("lib64"));
}

#[test]
pub(super) fn parent_references_in_members_are_not_installed() {
    for odd in [
        "cuda/../../evil",
        "top/lib/../x.so",
        "top/./lib/x.so",
        "top/lib/./x.so",
    ] {
        let files = vec![odd.to_string()];
        assert!(check_layout(&files).is_err(), "{odd}");
    }
    // The `..` must not reach the spec either: no `cp -a ..` line.
    let mut i = cudart();
    i.files = vec!["top/../../evil".into(), "top/LICENSE".into()];
    let spec = redist_component_spec(&i);
    assert!(!spec.contains("cp -a .."), "{spec}");
    assert!(!spec.contains("cp -a ."), "{spec}");
}

pub(super) fn owned(files: &[&str]) -> Vec<String> {
    files.iter().map(|s| s.to_string()).collect()
}

#[test]
pub(super) fn an_archive_whose_only_library_is_refused_is_not_a_package() {
    // The reviewer's case: the one library has a name the shell would
    // not take, so nothing is left to install (the license is no package).
    let files = owned(&["top/", "top/lib/", "top/lib/libfoo@1.so.1", "top/LICENSE"]);
    let layout = analyse(&files).unwrap();
    assert!(layout.members.is_empty());
    assert_eq!(layout.licenses, vec!["LICENSE".to_string()]);
    let err = check_layout(&files).unwrap_err();
    assert!(err.contains("no file to install"), "{err}");
    // The archive with a plain library next to it is a package, and the
    // refused one is absent from both the members and the spec.
    let mut i = cudart();
    i.files = owned(&[
        "top/",
        "top/lib/",
        "top/lib/libfoo@1.so.1",
        "top/lib/libok.so.1",
        "top/LICENSE",
    ]);
    assert!(check_layout(&i.files).is_ok());
    let spec = redist_component_spec(&i);
    assert!(spec.contains("libok.so.1"), "{spec}");
    assert!(!spec.contains("libfoo@"), "{spec}");
}
