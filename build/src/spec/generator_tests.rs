
use super::*;

/// "removed duplicate BuildRequires" appears in 20 of the 25 attempt
/// journals — py2pack emits the PyPI build requirements and we then add
/// `[build-system] requires` from the sdist on top.
#[test]
fn duplicate_build_requires_collapse_to_the_constrained_line() {
    let spec = "\
BuildRequires:  %{python_module pip}
BuildRequires:  %{python_module hatchling}
BuildRequires:  %{python_module setuptools >= 42}
BuildRequires:  %{python_module hatchling >= 1.0}
BuildRequires:  %{python_module setuptools}
BuildRequires:  fdupes
";
    let out = dedupe_build_requires(spec);
    assert_eq!(out.matches("hatchling").count(), 1, "{out}");
    assert!(out.contains("%{python_module hatchling >= 1.0}"), "{out}");
    assert_eq!(out.matches("setuptools").count(), 1, "{out}");
    assert!(out.contains("%{python_module setuptools >= 42}"), "{out}");
    assert!(out.contains("fdupes"), "non-python BR dropped");
    assert!(out.contains("pip"), "pip dropped");
}

#[test]
fn dedupe_respects_conditionals_multi_package_lines_and_distinct_constraints() {
    let spec = "\
BuildRequires:  gcc make
BuildRequires:  make
BuildRequires:  foo >= 1
BuildRequires:  foo < 2
BuildRequires:  foo >= 1
%if 0%{?suse_version} > 1500
BuildRequires:  bar
%else
BuildRequires:  bar
%endif
BuildRequires:  bar
BuildRequires:  baz
%if 1
BuildRequires:  baz
BuildRequires:  baz
%endif
";
    let out = dedupe_build_requires(spec);
    assert!(out.contains("BuildRequires:  gcc make\n"), "{out}");
    assert_eq!(out.matches("make").count(), 2, "{out}");
    assert_eq!(out.matches("foo >= 1").count(), 1, "{out}");
    assert_eq!(out.matches("foo < 2").count(), 1, "{out}");
    // both branches and the top level keep their own `bar`
    assert_eq!(out.matches("bar").count(), 3, "{out}");
    // top-level `baz` plus one inside the %if
    assert_eq!(out.matches("baz").count(), 2, "{out}");
}

#[test]
fn a_non_python_buildrequires_is_not_confused_with_the_macro_form() {
    // `cmake` the RPM and `%{python_module cmake}` are different packages.
    let spec = "\
BuildRequires:  cmake
BuildRequires:  %{python_module cmake}
";
    assert_eq!(dedupe_build_requires(spec), spec);
}

/// A top-level `tests/` directory scopes the default `%pytest`
/// invocation; without one, the old unscoped `%pytest` is kept.
#[test]
fn default_check_scopes_pytest_to_a_top_level_tests_dir() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let build_system = BuildSystem {
        requires: vec![],
        backend: None,
    };
    let spec = "\
BuildRequires:  %{python_module pip}
%changelog
";
    let scoped = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: true,
            has_c_extensions: false,
            top_level_tests_dir: Some("tests".to_string()),
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(scoped.contains("%check\n%pytest tests/\n"), "{scoped}");

    let unscoped = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: true,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(unscoped.contains("%check\n%pytest\n"), "{unscoped}");
}

fn dummy_release(homepage: &str, license: License) -> Release {
    Release {
        name: "pkg".to_string(),
        version: "1.0".to_string(),
        summary: Some("A package".to_string()),
        homepage: Some(homepage.to_string()),
        license,
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: String::new(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    }
}

/// The running example: NVIDIA's CUDA toolkit component wheels ship
/// compiled binaries under a manylinux platform tag — sitearch, not noarch.
#[test]
fn wheel_binary_skeleton_marks_platform_wheels_as_arch() {
    let release = dummy_release(
        "https://developer.nvidia.com/cuda-toolkit",
        License::FreeText("NVIDIA Proprietary".to_string()),
    );
    let wheel = "nvidia_cuda_nvcc_cu13-13.0.88-py3-none-manylinux_2_17_x86_64.whl";
    let spec = wheel_binary_skeleton(
        "nvidia-cuda-nvcc-cu13",
        "13.0.88",
        wheel,
        &release,
        Some(false),
        None,
    );

    assert!(spec.contains(&format!("Source0:        {wheel}")), "{spec}");
    assert!(
        !spec.contains("BuildArch:      noarch"),
        "platform wheel must not be noarch:\n{spec}"
    );
    assert!(spec.contains("%{buildroot}/%{$python_sitearch}"), "{spec}");
    assert!(spec.contains("%pyproject_install %{SOURCE0}"), "{spec}");
}

/// NVIDIA's real shape: arch-specific tag, but `Root-Is-Purelib: true`.
/// Not noarch (it ships `.so`), exclusive to the wheel's arch, and
/// installed under sitelib — where pip actually puts it.
#[test]
fn a_manylinux_purelib_wheel_is_arch_specific_but_installs_to_sitelib() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let wheel = "nvidia_curand-10.4.0.35-py3-none-manylinux_2_27_x86_64.whl";
    let spec = wheel_binary_skeleton(
        "nvidia-curand",
        "10.4.0.35",
        wheel,
        &release,
        Some(true),
        None,
    );
    assert!(!spec.contains("BuildArch:      noarch"), "{spec}");
    assert!(spec.contains("ExclusiveArch:  x86_64"), "{spec}");
    assert!(spec.contains("%{buildroot}/%{$python_sitelib}"), "{spec}");
    assert!(!spec.contains("python_sitearch"), "{spec}");
}

#[test]
fn a_multi_flavor_skeleton_installs_each_flavors_own_wheel() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let wheels = vec![
        (
            "python313".to_string(),
            "triton-3.8.0-cp313-cp313-manylinux_2_27_x86_64.whl".to_string(),
        ),
        (
            "python314".to_string(),
            "triton-3.8.0-cp314-cp314-manylinux_2_27_x86_64.whl".to_string(),
        ),
    ];
    let spec = wheel_multi_flavor_skeleton("triton", "3.8.0", &wheels, &release, Some(false), None);
    assert!(
        spec.contains("Source0:        triton-3.8.0-cp313-cp313"),
        "{spec}"
    );
    assert!(
        spec.contains("Source1:        triton-3.8.0-cp314-cp314"),
        "{spec}"
    );
    assert!(
        spec.contains("%python313_pyproject_install %{SOURCE0}"),
        "{spec}"
    );
    assert!(
        spec.contains("%python314_pyproject_install %{SOURCE1}"),
        "{spec}"
    );
    assert!(!spec.contains("%pyproject_install %{SOURCE0}"), "{spec}");
    assert!(spec.contains("ExclusiveArch:  x86_64"), "{spec}");
    assert!(spec.contains("%{buildroot}/%{$python_sitearch}"), "{spec}");
}

/// cuda-bindings / nccl4py: a namespace wheel with modules is not given the
/// `test -e` probe the validator rejects, and its licence comes from the
/// wheel when PyPI has none.
#[test]
fn a_namespace_wheel_gets_a_find_spec_check_and_the_wheels_licence() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Unknown);
    let layout = WheelLayout {
        namespace_module: Some("cuda.bindings".into()),
        first_shared_lib: Some("cuda/bindings/_internal/x.so".into()),
        license_expression: Some("LicenseRef-NVIDIA-SOFTWARE-LICENSE".into()),
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "cuda-bindings",
        "1.0",
        "cuda_bindings-1.0-cp313-abi3-manylinux_2_27_x86_64.whl",
        &release,
        Some(false),
        Some(&layout),
    );
    assert!(
            spec.contains(
                "$python -c \"import importlib.machinery as m, sys; s = m.PathFinder.find_spec('cuda', ['%{buildroot}%{$python_sitearch}']); s = s and m.PathFinder.find_spec('cuda.bindings', s.submodule_search_locations); sys.exit(0 if s else 1)\""
            ),
            "{spec}"
        );
    assert!(!spec.contains("test -e"), "{spec}");
    assert!(!spec.contains("smoke-test the import name"), "{spec}");
    assert!(spec.contains("License:        SUSE-NonFree"), "{spec}");
    assert!(!check_is_file_probe_only(&spec));
    // Older wheels: classifiers, then free text; an unknown text is not guessed.
    let from = |classifiers: &[&str], text: Option<&str>| {
        let l = WheelLayout {
            license_classifiers: classifiers.iter().map(|c| c.to_string()).collect(),
            license_text: text.map(str::to_string),
            ..Default::default()
        };
        wheel_license(&l)
    };
    assert_eq!(
        from(&["License :: OSI Approved :: MIT License"], None).as_deref(),
        Some("MIT")
    );
    // `BSD License` does not say 2 or 3 clauses: the free text may.
    assert_eq!(
        from(
            &["License :: OSI Approved :: BSD License"],
            Some("3-Clause BSD")
        )
        .as_deref(),
        Some("BSD-3-Clause")
    );
    assert_eq!(
        from(&[], Some("Apache License, Version 2.0")).as_deref(),
        Some("Apache-2.0")
    );
    assert_eq!(
        from(&[], Some("NVIDIA Proprietary Software")).as_deref(),
        Some("SUSE-NonFree")
    );
    assert_eq!(from(&[], Some("Custom terms, see LICENSE")), None);
    assert_eq!(from(&[], None), None);
    // The release's own licence wins over the wheel's.
    let own = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let spec = wheel_binary_skeleton(
        "p",
        "1",
        "p-1-py3-none-any.whl",
        &own,
        Some(true),
        Some(&layout),
    );
    assert!(spec.contains("License:        MIT"), "{spec}");
}

/// The generated `%check` must be one that works first time: importable
/// wheels import with the buildroot on PYTHONPATH inside %python_expand;
/// namespace/binary-only wheels assert their payload file instead.
#[test]
fn the_generated_check_imports_or_asserts_the_payload() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let wheel = "pkg-1.0-py3-none-any.whl";

    let importable = WheelLayout {
        importable: Some("pkg".into()),
        first_shared_lib: None,
        ..Default::default()
    };
    let spec = wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), Some(&importable));
    assert!(
            spec.contains(
                "%check\n%python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} $python -c \"import pkg\"\n"
            ),
            "{spec}"
        );
    assert!(!spec.contains("# TODO: smoke-test"), "{spec}");

    let binary_only = WheelLayout {
        importable: None,
        first_shared_lib: Some("nvidia/cu13/lib/libnvJitLink.so.13".into()),
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "nvidia-nvjitlink",
        "1.0",
        "nvidia_nvjitlink-1.0-py3-none-manylinux_2_27_x86_64.whl",
        &release,
        Some(true),
        Some(&binary_only),
    );
    assert!(
            spec.contains(
                "%check\n%python_expand test -e %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libnvJitLink.so.13\n"
            ),
            "{spec}"
        );

    // Unknown layout keeps the TODO rather than guessing.
    let spec = wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), None);
    assert!(spec.contains("# TODO: smoke-test"), "{spec}");
}

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
            "Source99:       python-open-webui.rpmlintrc
"
        ),
        "{spec}"
    );
    let (file, text) = wheel_rpmlintrc("open-webui", wheel, Some(&layout)).unwrap();
    assert_eq!(file, "python-open-webui.rpmlintrc");
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

/// see `is_prebuilt_binary_wheel`/`prebuilt_wheel_rpmlintrc`.
#[test]
fn a_binary_wheel_with_a_shared_lib_gets_a_pre_seeded_rpmlintrc() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let binary_only = WheelLayout {
        importable: None,
        first_shared_lib: Some("nvidia/cu13/lib/libfoo.so.1".into()),
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "nvidia-foo",
        "1.0",
        "nvidia_foo-1.0-py3-none-manylinux_2_27_x86_64.whl",
        &release,
        Some(true),
        Some(&binary_only),
    );
    assert!(
        spec.contains("Source99:       python-nvidia-foo.rpmlintrc\n"),
        "{spec}"
    );

    // A pure wheel, or one with no shared library, gets neither.
    let importable = WheelLayout {
        importable: Some("pkg".into()),
        first_shared_lib: None,
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "pkg",
        "1.0",
        "pkg-1.0-py3-none-any.whl",
        &release,
        Some(true),
        Some(&importable),
    );
    assert!(!spec.contains("Source99"), "{spec}");
}

/// The rpmlintrc keeps only what the package cannot fix, with the reason;
/// the executable bit, `python-base` and bundled headers are fixed in the spec.
#[test]
fn the_wheel_rpmlintrc_has_only_unfixable_filters_with_reasons() {
    use ap_ecosystem::sdist::WheelLayout;
    let rc = prebuilt_wheel_rpmlintrc(&WheelLayout::default());
    assert!(rc.contains("addFilter(\"missing-hash-section\")"), "{rc}");
    assert!(rc.contains("library-without-ldconfig-postin"), "{rc}");
    assert!(!rc.contains("shared-library-not-executable"), "{rc}");
    assert!(!rc.contains("no-dependency-on python-base"), "{rc}");
    assert!(!rc.contains("devel-file-in-non-devel-package"), "{rc}");
    assert!(!rc.contains("no-binary"), "{rc}");
    assert!(rc.matches('#').count() >= 2, "{rc}");
}

/// `libcusparseLt.so.0 (RUNPATH: $ORIGIN:)` is `binary-or-shlib-defines-rpath`
/// at badness 10000 and aborts rpmlint; but an `unused-rpmlintrc-filter` is an
/// error too, so the filter is written only for a wheel that has such a RUNPATH.
#[test]
fn the_origin_runpath_filter_is_written_only_when_a_library_has_one() {
    use ap_ecosystem::sdist::WheelLayout;
    let with = prebuilt_wheel_rpmlintrc(&WheelLayout {
        first_shared_lib: Some("nvidia/cusparselt/lib/libcusparseLt.so.0".into()),
        origin_runpath: true,
        ..Default::default()
    });
    assert!(
        with.contains("addFilter(\".*binary-or-shlib-defines-rpath.*[$]ORIGIN\")\n"),
        "{with}"
    );
    // It matches the `$ORIGIN` finding and no other RPATH value.
    let re = regex::Regex::new("binary-or-shlib-defines-rpath.*[$]ORIGIN").unwrap();
    assert!(re.is_match(
            "binary-or-shlib-defines-rpath /usr/lib/python3.13/site-packages/nvidia/libcusparseLt.so.0 $ORIGIN:"
        ));
    assert!(!re.is_match("binary-or-shlib-defines-rpath /usr/bin/x /opt/evil"));

    let without = prebuilt_wheel_rpmlintrc(&WheelLayout {
        first_shared_lib: Some("nvidia/cusparselt/lib/libcusparseLt.so.0".into()),
        ..Default::default()
    });
    assert!(!without.contains("rpath"), "{without}");
}

/// The pipeline writes the file only for a prebuilt binary wheel, and it
/// carries the layout's filters.
#[test]
fn the_rpmlintrc_file_follows_the_wheel_shape() {
    use ap_ecosystem::sdist::WheelLayout;
    let layout = WheelLayout {
        first_shared_lib: Some("p/libx.so.1".into()),
        origin_runpath: true,
        ..Default::default()
    };
    let (file, text) = wheel_rpmlintrc(
        "Nvidia-Foo",
        "nvidia_foo-1.0-py3-none-manylinux_2_27_x86_64.whl",
        Some(&layout),
    )
    .unwrap();
    assert_eq!(file, "python-nvidia-foo.rpmlintrc");
    assert!(text.contains("[$]ORIGIN"), "{text}");
    assert!(wheel_rpmlintrc("p", "p-1.0-py3-none-any.whl", Some(&layout)).is_none());
    assert!(
        wheel_rpmlintrc(
            "p",
            "p-1-py3-none-manylinux_2_27_x86_64.whl",
            Some(&WheelLayout::default())
        )
        .is_none()
    );
    assert!(wheel_rpmlintrc("p", "p-1-py3-none-manylinux_2_27_x86_64.whl", None).is_none());
}

/// python-triton's `proton`/`proton-viewer` and python-cuda-tile's
/// `cutile-cache`: the skeleton carries all of convention 18's pieces for
/// each wheel console script from the start, not after a failed build.
#[test]
fn wheel_console_scripts_get_the_alternatives_handling() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let layout = WheelLayout {
        importable: Some("triton".into()),
        console_scripts: vec!["proton".into(), "proton-viewer".into()],
        ..Default::default()
    };
    let spec = wheel_binary_skeleton(
        "triton",
        "3.0",
        "triton-3.0-py3-none-any.whl",
        &release,
        Some(true),
        Some(&layout),
    );
    for want in [
        "Requires:       python-base\nRequires(post):   update-alternatives\n\
             Requires(postun): update-alternatives\n%python_subpackages\n",
        "%pyproject_install %{SOURCE0}\n%python_clone -a %{buildroot}%{_bindir}/proton\n\
             %python_clone -a %{buildroot}%{_bindir}/proton-viewer\n# files without a shebang",
        "%post\n%python_install_alternative proton\n%python_install_alternative proton-viewer\n\n\
             %postun\n%python_uninstall_alternative proton\n%python_uninstall_alternative proton-viewer\n\n\
             %files %{python_files}\n",
        "%python_alternative %{_bindir}/proton\n%python_alternative %{_bindir}/proton-viewer\n\n%changelog",
    ] {
        assert!(spec.contains(want), "missing {want:?} in\n{spec}");
    }

    // Multi-flavor: the clone lines follow the per-flavor installs.
    let wheels = vec![
        (
            "python313".to_string(),
            "t-3.0-cp313-cp313-manylinux_2_17_x86_64.whl".to_string(),
        ),
        (
            "python314".to_string(),
            "t-3.0-cp314-cp314-manylinux_2_17_x86_64.whl".to_string(),
        ),
    ];
    let multi =
        wheel_multi_flavor_skeleton("t", "3.0", &wheels, &release, Some(false), Some(&layout));
    for want in [
        "%python313_pyproject_install %{SOURCE0}\n%python314_pyproject_install %{SOURCE1}\n\
             %python_clone -a %{buildroot}%{_bindir}/proton\n",
        "Requires(post):   update-alternatives\n",
        "%python_install_alternative proton-viewer\n",
        "%python_alternative %{_bindir}/proton\n",
    ] {
        assert!(multi.contains(want), "missing {want:?} in\n{multi}");
    }

    // No scripts: none of it appears, in either skeleton.
    let plain = WheelLayout::default();
    for spec in [
        wheel_binary_skeleton(
            "plain",
            "1.0",
            "plain-1.0-py3-none-any.whl",
            &release,
            Some(true),
            Some(&plain),
        ),
        wheel_binary_skeleton(
            "plain",
            "1.0",
            "p-1.0-py3-none-any.whl",
            &release,
            None,
            None,
        ),
        wheel_multi_flavor_skeleton("t", "3.0", &wheels, &release, Some(false), Some(&plain)),
    ] {
        for absent in [
            "update-alternatives",
            "python_clone",
            "%post",
            "python_alternative",
        ] {
            assert!(!spec.contains(absent), "{absent} in\n{spec}");
        }
    }
}

/// opencv-python-headless: auditwheel's hashed libraries are vendored under
/// `<name>.libs/`, so the generated Requires for their sonames must be
/// excluded or the package is uninstallable.
#[test]
fn a_wheel_with_hashed_libs_excludes_their_requires() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let layout = WheelLayout {
        importable: Some("cv2".into()),
        first_shared_lib: Some(
            "opencv_python_headless.libs/libavcodec-f34387fd.so.62.11.100".into(),
        ),
        has_hashed_libs: true,
        ..Default::default()
    };
    let wheel = "opencv_python_headless-4.12-cp37-abi3-manylinux_2_28_x86_64.whl";
    let spec = wheel_binary_skeleton(
        "opencv-python-headless",
        "4.12",
        wheel,
        &release,
        Some(false),
        Some(&layout),
    );
    // The hashed libraries and the driver's share the ONE regex.
    let line = format!(
        "%global __requires_exclude ^({}|{})$\n%python_subpackages\n",
        requires_exclude_inner(hashed_libs_requires_exclude()),
        requires_exclude_inner(crate::depclass::driver_requires_exclude())
    );
    assert!(spec.contains(&line), "{spec}");
    assert_eq!(spec.matches("__requires_exclude").count(), 1, "{spec}");
    let multi = wheel_multi_flavor_skeleton(
        "opencv-python-headless",
        "4.12",
        &[("python313".to_string(), wheel.to_string())],
        &release,
        Some(false),
        Some(&layout),
    );
    assert!(multi.contains(&line), "{multi}");

    // The regex matches hashed sonames only.
    let re = regex::Regex::new(hashed_libs_requires_exclude()).unwrap();
    for hashed in [
        "libavcodec-f34387fd.so.62.11.100()(64bit)",
        "libopenblasp-r0-37b5f859.3.3.so",
        "libaom-969ba5f6.so.3.13.1",
    ] {
        assert!(re.is_match(hashed), "{hashed}");
    }
    for ordinary in [
        "libcuda.so.1()(64bit)",
        "libz3.so.4.15",
        "libfoo-1.2.so",
        "libc.so.6",
    ] {
        assert!(!re.is_match(ordinary), "{ordinary}");
    }

    // No hashed libs: no exclude.
    let spec = wheel_binary_skeleton(
        "p",
        "1.0",
        "p-1.0-py3-none-any.whl",
        &release,
        Some(true),
        Some(&WheelLayout::default()),
    );
    assert!(!spec.contains("__requires_exclude"), "{spec}");
}

/// The regex and `is_hashed_soname` are one rule: both accept auditwheel
/// names and refuse date stamps, and agree on every name in between.
#[test]
fn the_hashed_libs_regex_agrees_with_is_hashed_soname() {
    let re = regex::Regex::new(hashed_libs_requires_exclude()).unwrap();
    let names = [
        "libfoo-1a2b3c4d.so.1",
        "libfoo-1a2b3c4d.so",
        "libavcodec-f34387fd.so.62.11.100",
        "libopenblasp-r0-37b5f859.3.3.so",
        "libfoo-deadbeef.so.1",
        "libfoo-0000000a.so.1",
        "libfoo-a0000000.so.1",
        "libfoo-20240101.so",
        "libfoo-20240101.so.1",
        "libfoo-00000000.so.1",
        "libfoo-1a2b3c4.so.1",
        "libfoo-1a2b3c4d5.so.1",
        "libfoo-1a2b3c4dx.so.1",
        "libfoo-1a2b3c4d.txt",
        "libfoo-1a2b3c4d",
        "libfoo-1a2b3c4dso",
        "libfoo-1.2.so",
        "libz3.so.4.15",
        "libcuda.so.1",
        "foo-1a2b3c4d.so.1",
        "libfoo-20240101-1a2b3c4d.so.2",
        "libfoo-1a2b3c4d.so.1.2-20240101",
        "libfoo-1A2B3C4D.so.1",
    ];
    for n in names {
        let sym = ap_ecosystem::sdist::is_hashed_soname(n);
        assert_eq!(re.is_match(n), sym, "{n}: regex vs predicate");
        // As rpm names the Requires of it.
        let req = format!("{n}()(64bit)");
        let req_ok = sym && (n.ends_with(".so") || n.contains(".so."));
        assert_eq!(re.is_match(&req), req_ok, "{req}");
    }
    assert!(ap_ecosystem::sdist::is_hashed_soname(
        "libfoo-1a2b3c4d.so.1"
    ));
    assert!(!ap_ecosystem::sdist::is_hashed_soname("libfoo-20240101.so"));
}

/// python-flashinfer-python: the wheel links libcuda.so.1, nothing
/// provides it, OBS reported it unresolvable.
#[test]
fn a_compiled_wheel_never_requires_the_nvidia_driver() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let spec = wheel_binary_skeleton(
        "flashinfer-python",
        "0.6",
        "flashinfer_python-0.6-cp39-abi3-manylinux_2_28_x86_64.whl",
        &release,
        Some(false),
        Some(&WheelLayout::default()),
    );
    let line = format!(
        "%global __requires_exclude {}\n%python_subpackages\n",
        crate::depclass::driver_requires_exclude()
    );
    assert!(spec.contains(&line), "{spec}");
    let re = regex::Regex::new(crate::depclass::driver_requires_exclude()).unwrap();
    for r in ["libcuda.so.1()(64bit)", "libnvidia-ml.so.1()(64bit)"] {
        assert!(re.is_match(r), "{r}");
    }
}

#[test]
fn the_driver_exclude_extends_an_existing_requires_exclude_once() {
    let spec = "Name: x\n%global __requires_exclude ^lib(mpi|ucp)\\.so.*$\n%python_subpackages\n";
    let out = with_driver_requires_exclude(spec);
    assert_eq!(out.matches("__requires_exclude").count(), 1, "{out}");
    let line = out
        .lines()
        .find(|l| l.contains("__requires_exclude"))
        .unwrap();
    let re = regex::Regex::new(line.trim_start_matches("%global __requires_exclude ")).unwrap();
    assert!(re.is_match("libmpi.so.40()(64bit)"), "{line}");
    assert!(re.is_match("libcuda.so.1()(64bit)"), "{line}");
    assert!(!re.is_match("libcudart.so.12()(64bit)"), "{line}");
    assert_eq!(with_driver_requires_exclude(&out), out);
    // Without any, it is added before %python_subpackages.
    let bare = with_driver_requires_exclude("Name: x\n%python_subpackages\n");
    assert!(
        bare.contains("%global __requires_exclude ^(libcuda"),
        "{bare}"
    );
    assert_eq!(with_driver_requires_exclude(&bare), bare);
}

#[test]
fn only_an_arch_package_naming_cuda_may_link_the_driver() {
    assert!(may_link_driver_library(
        "Name: x\nBuildRequires: cuda-nvcc\n"
    ));
    assert!(!may_link_driver_library(
        "Name: x\nBuildArch: noarch\nRequires: cuda\n"
    ));
    assert!(!may_link_driver_library("Name: x\n"));
}

#[test]
fn a_spec_with_the_looser_old_regex_is_rewritten() {
    let spec = "%global __requires_exclude ^lib.*-[0-9a-f]{8}.*\\.so.*$\n%python_subpackages\n";
    let out = with_hashed_libs_requires_exclude(spec);
    assert!(out.contains(hashed_libs_requires_exclude()), "{out}");
    assert!(!out.contains(OLD_HASHED_LIBS_REQUIRES_EXCLUDE), "{out}");
    assert_eq!(with_hashed_libs_requires_exclude(&out), out);
}

/// The prompt and the dependency reason text must not quote a regex of
/// their own: it would drift from the generated one.
#[test]
fn nothing_else_quotes_the_old_hashed_libs_regex() {
    for name in ["depclass", "agent"] {
        let text = module_source(name);
        assert!(!text.contains("[0-9a-f]{8}.*"), "{name} quotes a regex");
    }
}

/// The text of module `name`: `src/<name>.rs` and every `.rs` file under
/// `src/<name>/`, so that a module split into children is still whole.
fn module_source(name: &str) -> String {
    fn collect(dir: &std::path::Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&std::fs::read_to_string(&path).unwrap());
                out.push('\n');
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = std::fs::read_to_string(src.join(format!("{name}.rs"))).unwrap();
    collect(&src.join(name), &mut out);
    out
}

#[test]
fn c_project_skeleton_uses_cmake_macros() {
    let spec = c_project_skeleton(
        "libthing",
        "1.0",
        "https://example.org",
        "libthing-1.0",
        CBuildSystem::Cmake,
    );
    assert!(spec.contains("Name:           libthing"), "{spec}");
    assert!(spec.contains("%autosetup -n libthing-1.0"), "{spec}");
    assert!(spec.contains("BuildRequires:  cmake"), "{spec}");
    assert!(spec.contains("%cmake\n"), "{spec}");
    assert!(spec.contains("%cmake_build"), "{spec}");
    assert!(spec.contains("%cmake_install"), "{spec}");
    assert!(
        !spec.contains("autoreconf"),
        "cmake skeleton must not run autoreconf:\n{spec}"
    );
}

#[test]
fn c_project_skeleton_uses_autotools_macros() {
    let spec = c_project_skeleton(
        "libthing",
        "1.0",
        "https://example.org",
        "libthing-1.0",
        CBuildSystem::Autotools,
    );
    assert!(spec.contains("autoreconf -fiv"), "{spec}");
    assert!(spec.contains("%configure"), "{spec}");
    assert!(spec.contains("%make_build"), "{spec}");
    assert!(spec.contains("%make_install"), "{spec}");
    assert!(spec.contains("BuildRequires:  autoconf"), "{spec}");
    assert!(
        !spec.contains("%cmake"),
        "autotools skeleton must not reference cmake:\n{spec}"
    );
}

#[test]
fn a_monorepo_project_is_built_from_its_subdirectory() {
    let spec = "%build\n%pyproject_wheel\n\n%install\n%pyproject_install\n";
    let out = build_in_subdir(spec, "cuda_python");
    assert_eq!(
        out,
        "%build\ncd cuda_python\n%pyproject_wheel\n\n%install\ncd cuda_python\n%pyproject_install\n"
    );
}

#[test]
fn dist_info_always_uses_the_normalized_name() {
    let spec = "\
%files %{python_files}
%{python_sitelib}/rich-toolkit
%{python_sitelib}/rich-toolkit-%{version}.dist-info
";
    let out = normalize_files_paths(spec, "rich-toolkit");
    assert!(out.contains("%{python_sitelib}/rich_toolkit/"), "{out}");
    assert!(
        out.contains("%{python_sitelib}/rich_toolkit-%{version}.dist-info/"),
        "{out}"
    );
}

#[test]
fn a_bare_source_filename_follows_the_real_archive() {
    let mut release = ap_ecosystem::pypi::Release {
        name: "Events".into(),
        version: "0.5".into(),
        summary: None,
        homepage: None,
        license: ap_ecosystem::pypi::License::Unknown,
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: "events-0.5.tar.gz".into(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    };
    let spec =
        "Source:         Events-%{version}.zip\nBuildRequires:  unzip\nBuildRequires:  fdupes\n";
    assert_eq!(
        fix_bare_source_filename(spec, &release),
        "Source:         events-%{version}.tar.gz\nBuildRequires:  fdupes\n"
    );
    // A URL names where the file comes from; leave it.
    let url = "Source:         https://files.pythonhosted.org/x/Events-%{version}.zip\n";
    assert_eq!(fix_bare_source_filename(url, &release), url);
    // Nothing to say without a known archive.
    release.sdist.filename.clear();
    assert_eq!(fix_bare_source_filename(spec, &release), spec);
}

#[test]
fn trailing_slashes_do_not_double_the_dist_info_directory() {
    let spec = "\
%files
%{python_sitelib}/accelerate/
%{python_sitelib}/accelerate-%{version}.dist-info/
";
    let out = normalize_files_paths(spec, "accelerate");
    assert_eq!(
        out,
        "%files\n%{python_sitelib}/accelerate/\n%{python_sitelib}/accelerate-%{version}.dist-info/\n"
    );
    // Idempotent: a second pass changes nothing.
    assert_eq!(normalize_files_paths(&out, "accelerate"), out);
}

#[test]
fn a_module_name_that_is_not_the_package_name_is_left_alone() {
    // PyJWT installs as `jwt`; only the agent can know that from
    // top_level.txt, so the module line must survive untouched.
    let spec = "%files %{python_files}\n%{python_sitelib}/jwt/\n";
    assert_eq!(normalize_files_paths(spec, "PyJWT"), spec);
}

#[test]
fn a_script_named_like_the_package_is_not_made_a_directory() {
    let spec = "%files %{python_files}\n%{_bindir}/black\n\
                    %python_alternative %{_bindir}/black\n\
                    %{_bindir}/black/\n%{python_sitelib}/Black\n\
                    %{$python_sitelib}/Black/\n";
    let out = normalize_files_paths(spec, "Black");
    assert!(out.contains("\n%{_bindir}/black\n"), "{out}");
    assert!(
        out.contains("\n%python_alternative %{_bindir}/black\n"),
        "{out}"
    );
    assert!(out.contains("\n%{python_sitelib}/black/\n"), "{out}");
    assert!(out.contains("\n%{$python_sitelib}/black/\n"), "{out}");
}

#[test]
fn display_name_in_files_is_normalized() {
    let spec = "%files %{python_files}\n%{python_sitearch}/MarkupSafe\n";
    let out = normalize_files_paths(spec, "MarkupSafe");
    assert!(out.contains("%{python_sitearch}/markupsafe/"), "{out}");
}

#[test]
fn only_files_sections_are_rewritten() {
    let spec = "\
%install
  echo removing foo-bar.dist-info
%files
%{python_sitelib}/foo-bar-%{version}.dist-info/
%package doc
%description doc
%{python_sitelib}/foo-bar-%{version}.dist-info/
%files doc
%{python_sitelib}/foo-bar-%{version}.dist-info/
";
    let out = normalize_files_paths(spec, "foo-bar");
    assert!(
        out.contains("%install\n  echo removing foo-bar.dist-info\n"),
        "{out}"
    );
    assert!(
        out.contains("%files\n%{python_sitelib}/foo_bar-%{version}.dist-info/\n"),
        "{out}"
    );
    assert!(
        out.contains("%description doc\n%{python_sitelib}/foo-bar-%{version}.dist-info/\n"),
        "{out}"
    );
    assert!(
        out.contains("%files doc\n%{python_sitelib}/foo_bar-%{version}.dist-info/\n"),
        "{out}"
    );
}
