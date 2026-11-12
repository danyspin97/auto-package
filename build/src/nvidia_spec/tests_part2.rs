use super::tests::*;
use super::*;
use crate::spec::todo_placeholders;

#[test]
fn archive_members_must_agree_on_one_top_directory() {
    assert_eq!(
        top_dir(&owned(&["top/", "top/lib/x.so.1"])).as_deref(),
        Some("top")
    );
    // A leading `./` is no component, and the archive root is no member.
    assert_eq!(
        top_dir(&owned(&["./", "./top/lib/x.so.1", "top/LICENSE"])).as_deref(),
        Some("top")
    );
    // Two top directories: no top directory at all.
    assert_eq!(top_dir(&owned(&["top/lib/x.so.1", "other/LICENSE"])), None);
    assert!(analyse(&owned(&["top/lib/x.so.1", "other/LICENSE"])).is_none());
    // Only the first member used to count: a later one disagreeing refuses.
    assert_eq!(top_dir(&owned(&["top/lib/x.so.1", "./other/x.so"])), None);
    assert_eq!(top_dir(&owned(&["/abs/lib/x.so.1", "/abs/LICENSE"])), None);
}

#[test]
fn dot_slash_prefix_is_stripped_before_the_checks() {
    // tar listings carry `./`: the archive is still analysed, and the
    // result is the one a bare listing gives.
    let bare = vec![
        "top/lib/libx.so.1".to_string(),
        "top/include/x.h".to_string(),
        "top/LICENSE".to_string(),
    ];
    let dotted: Vec<String> = bare.iter().map(|f| format!("./{f}")).collect();
    assert!(analyse(&dotted).is_some());
    let mut a = cudart();
    a.files = bare.clone();
    let mut b = cudart();
    b.files = dotted;
    assert_eq!(redist_component_spec(&a), redist_component_spec(&b));
    // `./` directly on the top directory is the same archive too.
    let files = vec!["./cuda/lib/foo.so".to_string()];
    assert!(analyse(&files).is_some());
    assert_eq!(member_path("./././x"), "x");
    assert_eq!(member_path("cuda/../x"), "cuda/../x");
}

#[test]
fn deeper_member_directories_must_be_plain_too() {
    // The `.pc` sed line names every directory of a member, so an odd name
    // deeper than the first component keeps the member out of the package.
    for odd in [
        "top/lib/a b$(x)/libx.so.1",
        "top/lib/$(x)/y.pc",
        "top/lib/a\"b/y.pc",
        "top/lib/-x/y.pc",
    ] {
        let files = vec![odd.to_string()];
        assert!(check_layout(&files).is_err(), "{odd}");
    }
    let files = vec![
        "top/lib/pkgconfig/x.pc".to_string(),
        "top/lib/libx.so.1".to_string(),
    ];
    assert!(analyse(&files).is_some());
    // Through the spec, the odd directory name never reaches the output.
    let mut i = cudart();
    i.files = vec!["top/lib/a b$(x)/y.pc".into(), "top/LICENSE".into()];
    let spec = redist_component_spec(&i);
    assert!(!spec.contains("$(x)"), "{spec}");
}

#[test]
fn files_entry_quotes_and_escapes_what_rpm_would_split_or_expand() {
    assert_eq!(files_entry("a\"b"), "\"a\\\"b\"");
    assert_eq!(files_entry("a b"), "\"a b\"");
    assert_eq!(files_entry("100%"), "100%%");
    assert_eq!(files_entry("lib/x.so"), "lib/x.so");
}

#[test]
fn the_filesystem_package_owns_the_one_conf_file() {
    let spec = redist_filesystem_spec("13.4.3", "13.4.3");
    assert!(
        spec.contains("Name:           cuda-13-4-filesystem\n"),
        "{spec}"
    );
    assert!(spec.contains("BuildArch:      noarch\n"), "{spec}");
    assert!(spec.contains("Version:        13.4.3\n"), "{spec}");
    assert!(
        !spec.contains("Source0") && !spec.contains("%setup"),
        "{spec}"
    );
    assert!(
        spec.contains(
            "echo %{cuda_prefix}/lib64 > %{buildroot}%{_sysconfdir}/ld.so.conf.d/cuda-13-4.conf\n"
        ),
        "{spec}"
    );
    assert_eq!(
        section(&spec, "%files"),
        vec!["%config(noreplace) %{_sysconfdir}/ld.so.conf.d/cuda-13-4.conf"]
    );
    assert!(!spec.contains("TODO"), "{spec}");
    // The package a library component requires is the one named here.
    assert_eq!(filesystem_package("13.0"), "cuda-13-0-filesystem");
    assert!(
        redist_filesystem_rpmlintrc()
            .lines()
            .all(|l| l.starts_with('#'))
    );
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
    for f in [
        "etc/cufile.json",
        "lib64/libcufile.so.0",
        "lib64/libcufile_rdma.so.1",
    ] {
        let path = format!("{p}{f}");
        assert!(rt.contains(&path.as_str()), "{f}: {rt:?}");
    }
    assert!(
        spec.contains("%global __requires_exclude_from ^%{cuda_prefix}/(samples|tools|"),
        "{spec}"
    );
    // The RDMA plug-in's host libraries are Recommends, not Requires.
    assert!(
        spec.contains("|^%{cuda_prefix}/lib64/libcufile_rdma\\.so\n"),
        "{spec}"
    );
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
    for dep in [
        "cuda-cudart-devel",
        "cuda-cublas-devel",
        "cuda-cusparse-devel",
    ] {
        assert!(
            devel.contains(&format!("Requires:       {dep}\n")),
            "{devel}"
        );
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
    assert!(redist_rpmlintrc(&cudart()).contains("addFilter(\"dir-or-file-outside-snapshot\")"));
}

#[test]
fn a_component_spec_starts_with_the_header_of_the_clock_year() {
    let spec = crate::spec_tidy::header::with_year(2041, || {
        redist_component_spec(&input("cuda_nvcc", "13.4.48", NVCC))
    });
    assert!(
        spec.starts_with(
            "#\n# spec file for package cuda-nvcc\n#\n# Copyright (c) 2041 SUSE LLC and contributors\n"
        ),
        "{spec}"
    );
}
