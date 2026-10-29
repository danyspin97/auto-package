//! Tests of the rpmlintrc that a binary wheel gets and of its console scripts.

use super::*;

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
