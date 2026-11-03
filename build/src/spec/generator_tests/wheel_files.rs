//! Tests of the wheel payload: the repacked files, the layout, the headers and
//! the modules that a wheel installs.

use super::*;

#[test]
fn wheel_binary_skeleton_marks_pure_wheels_as_noarch() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let wheel = "somepkg-1.0-py3-none-any.whl";
    let spec = wheel_binary_skeleton("somepkg", "1.0", wheel, &release, Some(true), None);

    assert!(spec.contains("BuildArch:      noarch"), "{spec}");
    assert!(spec.contains("%{buildroot}/%{$python_sitelib}"), "{spec}");
}

/// open-webui's wheel repack tripped 238 `script-without-shebang` and 5
/// `zero-length` errors: every wheel repack normalises the modes in
/// `%install`, and a wheel with empty files gets exactly their filters.
#[test]
fn a_pure_wheel_repack_normalises_modes_and_filters_empty_files() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let wheel = "open_webui-1.0-py3-none-any.whl";
    let plain = wheel_binary_skeleton("open-webui", "1.0", wheel, &release, Some(true), None);
    assert!(plain.contains("-type f -perm /111 -exec sh -c"), "{plain}");
    assert!(
        plain.contains("%python_expand find %{buildroot}%{$python_sitelib} "),
        "{plain}"
    );
    assert!(plain.contains("ELF"), "{plain}");
    assert!(!plain.contains("Source99"), "{plain}");
    assert!(wheel_rpmlintrc("open-webui", wheel, None).is_none());

    let layout = WheelLayout {
        empty_files: vec![
            "open_webui/static/custom.css".into(),
            "open_webui/static/loader.js".into(),
            "weird name/x".into(),
        ],
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "open-webui",
        "1.0",
        wheel,
        &release,
        Some(true),
        Some(&layout),
    );
    assert!(
        spec.contains(
            "Source99:       python-open-webui-rpmlintrc
"
        ),
        "{spec}"
    );
    let (file, text) = wheel_rpmlintrc("open-webui", wheel, Some(&layout)).unwrap();
    assert_eq!(file, "python-open-webui-rpmlintrc");
    assert!(
        text.contains(
            r#"addFilter(r".*zero-length .*/site-packages/open_webui/static/custom\.css$")"#
        ),
        "{text}"
    );
    assert!(text.contains(r"loader\.js$"), "{text}");
    assert!(!text.contains("weird"), "{text}");
    assert!(!text.contains("missing-hash-section"), "{text}");
}

/// A prebuilt vendor `.so` wheel (nvidia-cublas's real shape) gets a
/// pre-seeded `Source99:`/rpmlintrc baseline instead of making the agent
/// re-derive the same five rpmlint false positives from a failed build —
fn layout_of(files: &[&str], requires: &[&str]) -> ap_ecosystem::sdist::WheelLayout {
    ap_ecosystem::sdist::WheelLayout {
        files: files.iter().map(|f| f.to_string()).collect(),
        dist_info: files
            .iter()
            .find_map(|f| f.split('/').next().filter(|t| t.ends_with(".dist-info")))
            .map(str::to_string),
        requires_dist: requires.iter().map(|r| r.to_string()).collect(),
        importable: Some("x".into()),
        ..Default::default()
    }
}

/// NVIDIA's shape (cublas, cufft, nvjitlink ...): a namespace shared with
/// other wheels, headers removed in %install, one licence in the dist-info.
/// Every one of those specs had its %files written by hand.
/// A hand-built layout (not from `wheel_layout`, which already filters)
/// with a header directory that would run Lua or the shell in `%install`:
/// the directory is left out of the cleanup, and so is every header in it.
#[test]
fn unsafe_header_dirs_are_never_written_into_install() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    for bad in [
        "nvidia/%{lua:os.execute(\"id\")}",
        "nvidia/a;touch x",
        "nvidia/$(x)",
        "nvidia/a b",
    ] {
        let mut layout = layout_of(
            &[
                "nvidia/cu13/lib/libnvJitLink.so.13",
                "nvidia/cu13/include/nvJitLink.h",
                "nvidia_nvjitlink-1.0.dist-info/METADATA",
            ],
            &[],
        );
        layout.header_dirs = vec![bad.into(), "nvidia/cu13/include".into()];
        let spec = wheel_binary_skeleton(
            "nvidia-nvjitlink",
            "1.0",
            "nvidia_nvjitlink-1.0-py3-none-manylinux_2_27_x86_64.whl",
            &release,
            Some(true),
            Some(&layout),
        );
        assert!(!spec.contains("%{lua:os"), "{bad}: {spec}");
        assert!(!spec.contains(";touch"), "{bad}: {spec}");
        assert!(!spec.contains("$(x)"), "{bad}: {spec}");
        assert!(
            !spec.contains("rm -rf %{buildroot}%{$sitedir}/nvidia/a"),
            "{bad}"
        );
        // The safe directory is still cleaned up.
        assert!(spec.contains("/nvidia/cu13/include"), "{bad}: {spec}");
    }
}

#[test]
fn a_namespace_wheel_lists_its_files_without_a_todo() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let mut layout = layout_of(
        &[
            "nvidia/cu13/lib/libnvJitLink.so.13",
            "nvidia/cu13/include/nvJitLink.h",
            "nvidia_nvjitlink-1.0.dist-info/METADATA",
            "nvidia_nvjitlink-1.0.dist-info/RECORD",
            "nvidia_nvjitlink-1.0.dist-info/licenses/License.txt",
        ],
        &["nvidia-cuda-nvrtc", "nvidia-foo (>=2.0); extra == \"x\""],
    );
    layout.header_dirs = vec!["nvidia/cu13/include".into()];
    let spec = wheel_binary_skeleton(
        "nvidia-nvjitlink",
        "1.0",
        "nvidia_nvjitlink-1.0-py3-none-manylinux_2_27_x86_64.whl",
        &release,
        Some(true),
        Some(&layout),
    );
    let files = spec.split("%files %{python_files}\n").nth(1).unwrap();
    let files = files.split("%changelog").next().unwrap();
    assert_eq!(
        files.trim(),
        "%dir %{python_sitelib}/nvidia\n\
             %dir %{python_sitelib}/nvidia/cu13\n\
             %{python_sitelib}/nvidia/cu13/lib/\n\
             %exclude %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/licenses/License.txt\n\
             %license %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/licenses/License.txt\n\
             %{python_sitelib}/nvidia_nvjitlink-1.0.dist-info/"
    );
    assert!(!spec.contains("TODO"), "{spec}");
    // The wheel's own unconditional dependency, and not its extra.
    assert!(
        spec.contains("Requires:       python-nvidia-cuda-nvrtc\n"),
        "{spec}"
    );
    assert!(!spec.contains("nvidia-foo"), "{spec}");
}

/// nccl-extensions installs `nccl/ep/` and requires nccl4py, which owns the
/// regular package `nccl`: with nccl4py in the build environment
/// `find_spec('nccl.ep')` answered `None` and `%check` failed. The generated
/// snippet looks in the buildroot only; run it against both trees.
#[test]
fn the_namespace_check_is_not_hidden_by_a_sibling_regular_package() {
    let snippet = namespace_find_spec_snippet("nccl.ep", "/tmp/root");
    assert_eq!(
        snippet,
        "import importlib.machinery as m, sys; s = m.PathFinder.find_spec('nccl', ['/tmp/root']); \
             s = s and m.PathFinder.find_spec('nccl.ep', s.submodule_search_locations); \
             sys.exit(0 if s else 1)"
    );
    let Ok(python) = std::process::Command::new("python3")
        .arg("--version")
        .output()
    else {
        return;
    };
    assert!(python.status.success());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let system = dir.path().join("system");
    std::fs::create_dir_all(root.join("nccl/ep")).unwrap();
    std::fs::write(root.join("nccl/ep/__init__.py"), "").unwrap();
    std::fs::create_dir_all(system.join("nccl")).unwrap();
    std::fs::write(system.join("nccl/__init__.py"), "").unwrap();
    let run = |module: &str| {
        std::process::Command::new("python3")
            .current_dir(dir.path())
            .env(
                "PYTHONPATH",
                format!("{}:{}", root.display(), system.display()),
            )
            .args([
                "-c",
                &namespace_find_spec_snippet(module, &root.display().to_string()),
            ])
            .status()
            .unwrap()
            .success()
    };
    assert!(run("nccl.ep"));
    assert!(!run("nccl.missing"));
    assert!(!run("other.ep"));
    // What it replaces fails on the same tree.
    let old = std::process::Command::new("python3")
        .current_dir(dir.path())
        .env(
            "PYTHONPATH",
            format!("{}:{}", root.display(), system.display()),
        )
        .args([
            "-c",
            "import importlib.util as u, sys; sys.exit(0 if u.find_spec('nccl.ep') else 1)",
        ])
        .status()
        .unwrap();
    assert!(!old.success());
}

/// The `%install` and `%files` of a wheel, from the skeleton of `layout`.
fn wheel_install_and_files(
    layout: &ap_ecosystem::sdist::WheelLayout,
    purelib: bool,
) -> (String, String, String) {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let spec = wheel_binary_skeleton(
        "pkg",
        "1.0",
        "pkg-1.0-py3-none-manylinux_2_27_x86_64.whl",
        &release,
        Some(purelib),
        Some(layout),
    );
    let part = |from: &str, to: &str| {
        let tail = spec.split(from).nth(1).unwrap_or("");
        tail.split(to).next().unwrap_or("").to_string()
    };
    (
        part("%install\n", "%check\n"),
        part("%check\n", "%files"),
        part("%files %{python_files}\n", "%changelog"),
    )
}

/// cuda-bindings: `cuda/bindings/_lib/` holds one header AND the regular
/// package `cuda.bindings._lib` (an `__init__.py`, Cython sources). Removing
/// the directory broke the import; only the header goes, and `%files` does
/// not list it.
#[test]
fn a_header_directory_that_holds_python_loses_only_its_headers() {
    let mut layout = layout_of(
        &[
            "cuda/bindings/__init__.py",
            "cuda/bindings/_lib/__init__.py",
            "cuda/bindings/_lib/utils.pxd",
            "cuda/bindings/_lib/utils.h",
            "cuda/bindings/_internal/nvml.so",
            "pkg-1.0.dist-info/METADATA",
        ],
        &[],
    );
    layout.importable = None;
    layout.first_shared_lib = Some("cuda/bindings/_internal/nvml.so".into());
    layout.header_dirs = vec!["cuda/bindings/_lib".into()];
    let (install, _, files) = wheel_install_and_files(&layout, false);
    assert!(
            install.contains(
                "find %{buildroot}%{$python_sitearch}/cuda/bindings/_lib -type f \\( -name '*.h' -o -name '*.hpp' \\) -delete\n"
            ),
            "{install}"
        );
    assert!(!install.contains("rm -rf"), "{install}");
    assert!(
        files.contains("%{python_sitearch}/cuda/bindings/\n"),
        "{files}"
    );

    // A directory of headers and extension-less headers only is still removed whole.
    let mut headers = layout_of(
        &[
            "p/lib/libx.so.1",
            "p/include/a.h",
            "p/include/b.cuh",
            "p/include/cuda/atomic",
            "pkg-1.0.dist-info/METADATA",
        ],
        &[],
    );
    headers.header_dirs = vec!["p/include".into()];
    let (install, _, _) = wheel_install_and_files(&headers, true);
    assert!(
        install.contains("rm -rf %{buildroot}%{$python_sitelib}/p/include\n"),
        "{install}"
    );

    // In a namespace directory the remaining files are listed one by one,
    // without the removed headers.
    let mut mixed = layout_of(
        &[
            "ns/data/a.h",
            "ns/data/b.txt",
            "ns/data/sub/c.txt",
            "ns/lib/libx.so.1",
            "pkg-1.0.dist-info/METADATA",
        ],
        &[],
    );
    mixed.header_dirs = vec!["ns/data".into()];
    let (_, _, files) = wheel_install_and_files(&mixed, true);
    assert!(
        files.contains("%{python_sitelib}/ns/data/b.txt\n"),
        "{files}"
    );
    assert!(files.contains("%{python_sitelib}/ns/data/sub/"), "{files}");
    assert!(!files.contains("a.h"), "{files}");
}

/// nvidia-cuda-crt is `include/crt/*.h` and nothing else: removing the
/// headers left an empty package and a `%files` TODO. The headers are the
/// payload there, and stay (the review splits them into a `-devel`).
#[test]
fn a_header_library_wheel_keeps_its_headers() {
    let mut layout = layout_of(
        &[
            "nvidia/cu13/include/crt/mma.h",
            "nvidia/cu13/include/crt/mma.hpp",
            "pkg-1.0.dist-info/METADATA",
            "pkg-1.0.dist-info/licenses/License.txt",
        ],
        &[],
    );
    layout.importable = None;
    layout.header_dirs = vec!["nvidia/cu13/include/crt".into()];
    let (install, check, files) = wheel_install_and_files(&layout, true);
    assert!(!install.contains("rm -rf"), "{install}");
    assert!(!install.contains("-delete"), "{install}");
    assert!(
        install.contains("belong in a\n# -devel package"),
        "{install}"
    );
    assert_eq!(
        check,
        "%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/include/crt/mma.h\n\n"
    );
    assert!(!files.contains("TODO"), "{files}");
    assert!(
        files.contains("%{python_sitelib}/nvidia/cu13/include/crt/\n"),
        "{files}"
    );
    // Executables next to the headers make it a toolchain, not a header library.
    let mut toolchain = layout.clone();
    toolchain.files.push("nvidia/cu13/bin/nvcc".into());
    let (install, _, _) = wheel_install_and_files(&toolchain, true);
    assert!(
        install.contains("rm -rf %{buildroot}%{$python_sitelib}/nvidia/cu13/include/crt\n"),
        "{install}"
    );
}

/// nvidia-cuda-nvdisasm (an executable under `bin/`), cuda-toolkit (only a
/// dist-info) and nvidia-cutlass-dsl (a top-level `.pth`): each had a `%check`
/// and `%files` TODO although the file list says what they install.
#[test]
fn a_wheel_without_modules_or_libraries_gets_a_check_and_files() {
    let mut bin = layout_of(
        &["nvidia/cu13/bin/nvdisasm", "pkg-1.0.dist-info/METADATA"],
        &[],
    );
    bin.importable = None;
    let (_, check, files) = wheel_install_and_files(&bin, true);
    assert_eq!(
        check,
        "%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/bin/nvdisasm\n\n"
    );
    assert!(
        files.contains("%{python_sitelib}/nvidia/cu13/bin/\n"),
        "{files}"
    );
    assert!(!files.contains("TODO"), "{files}");

    let mut meta = layout_of(
        &[
            "pkg-1.0.dist-info/METADATA",
            "pkg-1.0.dist-info/WHEEL",
            "pkg-1.0.dist-info/RECORD",
        ],
        &[],
    );
    meta.importable = None;
    let (_, check, files) = wheel_install_and_files(&meta, true);
    assert_eq!(
        check,
        "%python_expand test -d %{buildroot}%{$python_sitelib}/pkg-1.0.dist-info\n\n"
    );
    assert_eq!(files.trim(), "%{python_sitelib}/pkg-1.0.dist-info/");

    let mut pth = layout_of(
        &[
            "pkg_packages.pth",
            "pkg-1.0.dist-info/METADATA",
            "pkg-1.0.dist-info/licenses/LICENSE",
        ],
        &[],
    );
    pth.importable = None;
    let (_, check, files) = wheel_install_and_files(&pth, true);
    assert!(
        check.contains("test -e %{buildroot}%{$python_sitelib}/pkg_packages.pth"),
        "{check}"
    );
    assert!(
        files.starts_with("%{python_sitelib}/pkg_packages.pth\n%exclude"),
        "{files}"
    );
    assert!(!files.contains("TODO"), "{files}");
}

/// nvidia-cutlass-dsl-libs-base: `_mlir/` has modules but no `__init__.py`
/// here (the sibling wheel owns the package), so its files are listed one by
/// one, and the `__pycache__` the build writes beside them has to be listed
/// too or the build ends in "Installed (but unpackaged) file(s)".
#[test]
fn modules_listed_one_by_one_bring_their_pycache() {
    let mut layout = layout_of(
        &[
            "ns/pkg/_mlir/ir.py",
            "ns/pkg/_mlir/_mlir_libs/_x.so",
            "ns/pkg/_mlir/dialects/_a_gen.py",
            "ns/pkg/_mlir/dialects/_b_gen.py",
            "pkg-1.0.dist-info/METADATA",
        ],
        &[],
    );
    layout.importable = None;
    let (_, _, files) = wheel_install_and_files(&layout, false);
    assert_eq!(
        files.trim(),
        "%dir %{python_sitearch}/ns\n\
             %dir %{python_sitearch}/ns/pkg\n\
             %dir %{python_sitearch}/ns/pkg/_mlir\n\
             %{python_sitearch}/ns/pkg/_mlir/ir.py\n\
             %{python_sitearch}/ns/pkg/_mlir/__pycache__/\n\
             %{python_sitearch}/ns/pkg/_mlir/_mlir_libs/\n\
             %{python_sitearch}/ns/pkg/_mlir/dialects/\n\
             %{python_sitearch}/pkg-1.0.dist-info/"
    );
}

#[test]
fn a_package_directory_is_listed_whole_and_an_odd_layout_keeps_the_todo() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let pkg = layout_of(
        &[
            "pkg/__init__.py",
            "pkg/sub/x.py",
            "pkg-1.0.dist-info/METADATA",
        ],
        &[],
    );
    let spec = wheel_binary_skeleton(
        "pkg",
        "1.0",
        "pkg-1.0-py3-none-any.whl",
        &release,
        Some(true),
        Some(&pkg),
    );
    assert!(
        spec.contains(
            "%files %{python_files}\n%{python_sitelib}/pkg/\n%{python_sitelib}/pkg-1.0.dist-info/\n"
        ),
        "{spec}"
    );
    // A top-level module (its own __pycache__) or a .data tree is not listed blind.
    for odd in [
        &["mod.py", "mod-1.0.dist-info/METADATA"][..],
        &[
            "pkg/__init__.py",
            "pkg-1.0.data/scripts/tool",
            "pkg-1.0.dist-info/METADATA",
        ][..],
    ] {
        let layout = layout_of(odd, &[]);
        let spec = wheel_binary_skeleton(
            "pkg",
            "1.0",
            "pkg-1.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&layout),
        );
        assert!(spec.contains("# TODO: list the exact paths"), "{spec}");
    }
}
