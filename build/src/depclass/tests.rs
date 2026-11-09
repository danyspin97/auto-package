use super::*;

fn none(_: &str) -> bool {
    false
}

fn built(raw: &str) -> Option<(String, PackageKind)> {
    match triage(raw, &none) {
        Triage::Build { name, kind } => Some((name, kind)),
        Triage::Skip(_) => None,
    }
}

fn skipped(raw: &str) -> Skip {
    match triage(raw, &none) {
        Triage::Skip(s) => s,
        Triage::Build { name, .. } => panic!("{raw} would build {name}"),
    }
}

#[test]
fn a_translated_name_keeps_the_native_kind_of_its_original() {
    let originals = vec!["libtrantor-devel".to_string(), "python313-foo".to_string()];
    let names = vec!["trantor".to_string(), "foo".to_string()];
    assert_eq!(native_among(&originals, &names), ["trantor"]);
    let ffmpeg = vec!["ffmpeg-8-devel".to_string()];
    assert_eq!(native_among(&ffmpeg, &["ffmpeg".to_string()]), ["ffmpeg"]);
    assert!(native_among(&[], &names).is_empty());
}

#[test]
fn python_names_lose_their_flavor_prefix() {
    for raw in ["python313-foo", "python3-foo", "python-foo"] {
        assert_eq!(
            built(raw),
            Some(("foo".into(), PackageKind::Python)),
            "{raw}"
        );
    }
    assert_eq!(
        built("python3dist(foo-bar)"),
        Some(("foo-bar".into(), PackageKind::Python))
    );
    assert_eq!(
        built("python313dist(foo)"),
        Some(("foo".into(), PackageKind::Python))
    );
}

#[test]
fn devel_packages_name_the_upstream_library() {
    assert_eq!(
        built("libtrantor-devel"),
        Some(("trantor".into(), PackageKind::Native))
    );
    assert_eq!(built("numpy"), Some(("numpy".into(), PackageKind::Unknown)));
}

#[test]
fn the_interpreter_and_its_subpackages_are_never_built() {
    for raw in [
        "python3-devel",
        "python313-base",
        "python-rpm-macros",
        "python3",
        "python313",
    ] {
        assert!(matches!(skipped(raw), Skip::Interpreter(_)), "{raw}");
    }
    // A PyPI project that merely starts with a stem is still one.
    assert!(built("python313-tools-extra").is_some());
    assert!(built("python313-docker").is_some());
}

#[test]
fn the_stdlib_subpackages_of_cpython_are_interpreter_capabilities() {
    // Each is a subpackage of Factory's python313 (`python313:base`,
    // `python313:doc`, `python313`); none is a PyPI project of that name.
    for raw in [
        "python-dbm",
        "python3-dbm",
        "python313-dbm",
        "python314-dbm",
        "python313-curses",
        "python313-idle",
        "python313-tk",
        "python313-doc-devhelp",
        "python313-testsuite",
        "python313-tools",
    ] {
        assert!(matches!(skipped(raw), Skip::Interpreter(_)), "{raw}");
    }
    assert!(built("python-dbm-extra").is_some());
}

#[test]
fn a_system_tool_alias_is_skipped_under_any_spelling() {
    assert!(matches!(skipped("python313-ninja"), Skip::SystemTool(_)));
    assert!(matches!(skipped("ninja"), Skip::SystemTool(_)));
}

/// The regex and `is_driver_soname` are one rule.
#[test]
fn the_driver_regex_agrees_with_is_driver_soname() {
    let re = regex::Regex::new(driver_requires_exclude()).unwrap();
    for n in [
        "libcuda.so.1()(64bit)",
        "libcuda.so.1",
        "libcuda.so",
        "libnvidia-ml.so.1()(64bit)",
        "libnvidia-ptxjitcompiler.so.1()(64bit)",
        "libnvidia-nvvm.so.4",
        "libnvcuvid.so.1",
        "libcudadebugger.so.1()(64bit)",
        "libcudart.so.12()(64bit)",
        "libcudnn.so.9()(64bit)",
        "libcudart.so.13",
        "libnvrtc.so.12()(64bit)",
        "libnvidia-.so.1",
        "libcudaq.so",
        "libc.so.6()(64bit)",
    ] {
        assert_eq!(re.is_match(n), is_driver_soname(n), "{n}");
    }
    assert!(is_driver_soname("libcuda.so.1()(64bit)"));
    assert!(!is_driver_soname("libcudart.so.12()(64bit)"));
    assert!(!is_driver_soname("libnvrtc.so.12()(64bit)"));
}

#[test]
fn a_vendor_driver_library_is_unbuildable() {
    for raw in [
        "libcuda.so.1()(64bit)",
        "libcuda.so.1",
        "libnvidia-ml.so.1()(64bit)",
        "libnvidia-ptxjitcompiler.so.1()(64bit)",
        "libnvcuvid.so.1()(64bit)",
    ] {
        let s = skipped(raw);
        assert!(matches!(s, Skip::Driver(_)), "{raw}");
        assert!(s.is_unbuildable());
    }
}

/// python-opencv-python-headless bundles `libavcodec-f34387fd.so.62...`;
/// dependents of it were reported missing `avcodec-f34387fd` and the like.
#[test]
fn a_hashed_library_bundled_in_a_wheel_is_never_a_package() {
    for raw in [
        "libavcodec-f34387fd.so.62.11.100()(64bit)",
        "libavcodec-f34387fd.so.62.11.100(LIBAVCODEC_62)(64bit)",
        "libopenblasp-r0-37b5f859.3.3.so()(64bit)",
        "libquadmath-96973f99.so.0.0.0()(64bit)",
    ] {
        let s = skipped(raw);
        assert!(matches!(s, Skip::NotAPackage { .. }), "{raw}");
        assert!(s.is_unbuildable());
    }
    // Ordinary sonames are untouched.
    assert!(built("libfoo-1.2.so.3()(64bit)").is_some());
    assert!(built("libz3.so.4.15()(64bit)").is_some());
    // A date stamp is not a hash.
    assert!(built("libfoo-20240101.so.1()(64bit)").is_some());
}

#[test]
fn a_soname_of_another_library_is_built_under_its_upstream_name() {
    assert_eq!(
        built("libfoo.so.2()(64bit)"),
        Some(("foo".into(), PackageKind::Native))
    );
    // Too short to search for once `lib` is gone.
    assert_eq!(
        built("libz.so.1"),
        Some(("libz".into(), PackageKind::Native))
    );
}

#[test]
fn pkgconfig_and_cmake_capabilities_name_a_native_project() {
    assert_eq!(
        built("pkgconfig(libuv)"),
        Some(("libuv".into(), PackageKind::Native))
    );
    assert_eq!(
        built("cmake(Eigen3)"),
        Some(("Eigen3".into(), PackageKind::Native))
    );
}

#[test]
fn files_and_rpm_features_are_not_packages() {
    for raw in [
        "/usr/bin/foo",
        "perl(Foo::Bar)",
        "rpmlib(PayloadIsZstd)",
        "weird(thing)",
    ] {
        let s = skipped(raw);
        assert!(matches!(s, Skip::NotAPackage { .. }), "{raw}");
        assert!(s.is_unbuildable());
    }
}

#[test]
fn what_the_distro_provides_is_skipped_not_built() {
    let provides = |c: &str| matches!(c, "pkgconfig(zlib)" | "libpng" | "libfoo-devel");
    assert_eq!(
        triage("pkgconfig(zlib)", &provides),
        Triage::Skip(Skip::Distro("pkgconfig(zlib)".into()))
    );
    // The bare name behind a devel wrapper.
    assert_eq!(
        triage("libpng-devel", &provides),
        Triage::Skip(Skip::Distro("libpng".into()))
    );
    // The devel package's own name counts as the distro having it.
    assert_eq!(
        triage("libfoo-devel", &provides),
        Triage::Skip(Skip::Distro("libfoo-devel".into()))
    );
    // A PyPI project is not looked up as a capability.
    let always = |_: &str| true;
    assert!(matches!(
        triage("python313-six", &always),
        Triage::Build { .. }
    ));
    // Distro-provided capabilities are satisfied, not unbuildable.
    assert!(!Skip::Distro("x".into()).is_unbuildable());
}

#[test]
fn triage_all_collapses_flavors_and_keeps_the_reasons() {
    let raw: Vec<String> = [
        "python313-cuda-bindings",
        "python314-cuda-bindings",
        "python3-cuda-bindings",
        "python3-devel",
        "libcuda.so.1()(64bit)",
        "  ",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let (build, skipped) = triage_all(&raw, &none);
    assert_eq!(build, vec!["cuda-bindings"]);
    let names: Vec<&str> = skipped.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["python3-devel", "libcuda.so.1()(64bit)"]);
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn availability_prefers_the_built_own_spelling_and_the_own_project() {
    let own = set(&["python-foo_bar", "python-foo-bar"]);
    let statuses: HashMap<String, BuildStatus> = [
        ("python-foo_bar".to_string(), BuildStatus::Failed),
        ("python-foo-bar".to_string(), BuildStatus::Succeeded),
    ]
    .into();
    let linked = vec![(
        "openSUSE:Factory".to_string(),
        Arc::new(set(&["python-foo-bar"])),
    )];
    assert_eq!(
        availability("foo.bar", &own, &statuses, &linked),
        Availability::InProject {
            pkg: "python-foo-bar".into(),
            status: Some(BuildStatus::Succeeded)
        }
    );
}

#[test]
fn availability_keeps_a_failed_own_package_visible_and_finds_linked_ones() {
    let own = set(&["python-bad"]);
    let statuses: HashMap<String, BuildStatus> =
        [("python-bad".to_string(), BuildStatus::Failed)].into();
    let linked = vec![(
        "openSUSE:Factory".to_string(),
        Arc::new(set(&["libpng16", "python-six"])),
    )];
    assert_eq!(
        availability("bad", &own, &statuses, &linked),
        Availability::InProject {
            pkg: "python-bad".into(),
            status: Some(BuildStatus::Failed)
        }
    );
    assert_eq!(
        availability("six", &own, &statuses, &linked),
        Availability::InLinked {
            project: "openSUSE:Factory".into(),
            pkg: "python-six".into()
        }
    );
    assert_eq!(
        availability("nothing", &own, &statuses, &linked),
        Availability::Absent
    );
}
