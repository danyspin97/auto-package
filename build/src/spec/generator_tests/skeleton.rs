//! Tests of the wheel, namespace and C project skeletons.

use super::*;

pub(super) fn dummy_release(homepage: &str, license: License) -> Release {
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
fn c_project_skeleton_uses_cmake_macros() {
    let spec = c_project_skeleton(
        "libthing",
        "1.0",
        "https://example.org",
        "libthing-1.0",
        CBuildSystem::Cmake,
        None,
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
        None,
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
fn c_project_skeleton_uses_meson_macros() {
    let spec = c_project_skeleton(
        "fwupd-efi",
        "1.8",
        "https://example.org",
        "fwupd-efi-1.8",
        CBuildSystem::Meson,
        None,
    );
    assert!(spec.contains("BuildRequires:  meson"), "{spec}");
    assert!(spec.contains("%meson\n%meson_build"), "{spec}");
    assert!(spec.contains("%meson_install"), "{spec}");
    assert!(!spec.contains("%cmake"), "{spec}");
    assert!(!spec.contains("\ncd "), "no subdirectory, no cd:\n{spec}");
}

#[test]
fn c_project_skeleton_changes_into_the_build_subdirectory() {
    let spec = c_project_skeleton(
        "fortune-mod",
        "2.10.0",
        "https://example.org",
        "fortune-mod-2.10.0",
        CBuildSystem::Cmake,
        Some("fortune-mod"),
    );
    assert!(spec.contains("%build\ncd fortune-mod\n%cmake\n"), "{spec}");
    assert!(
        spec.contains("%install\ncd fortune-mod\n%cmake_install\n"),
        "{spec}"
    );
}
