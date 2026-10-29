use super::*;
use crate::redist::TestMode;

fn build(raw: &str) -> Option<(String, PackageKind)> {
    match triage(raw, &|_| false) {
        Triage::Build { name, kind } => Some((name, kind)),
        Triage::Skip(_) => None,
    }
}

#[test]
fn a_cuda_soname_is_a_package_to_build_only_with_the_mode_on() {
    {
        let _off = TestMode::set(false);
        // As before: the library is a vendor one, never a package.
        assert_eq!(build("libcublas.so.13()(64bit)"), None);
        assert!(matches!(
            triage("libcublas.so.13()(64bit)", &|_| false),
            Triage::Skip(Skip::Vendor(_))
        ));
    }
    let _on = TestMode::set(true);
    for soname in [
        "libcublas.so.13()(64bit)",
        "libcublasLt.so.13()(64bit)",
        "libcublas.so.13",
    ] {
        assert_eq!(
            build(soname),
            Some(("cuda-cublas".to_string(), PackageKind::Native)),
            "{soname}"
        );
    }
    assert_eq!(
        build("libcudart.so.13()(64bit)"),
        Some(("cuda-cudart".to_string(), PackageKind::Native))
    );
    assert_eq!(
        build("libcudnn.so.9()(64bit)"),
        Some(("cuda-cudnn".to_string(), PackageKind::Native))
    );
}

#[test]
fn the_driver_stays_unbuildable_with_the_mode_on() {
    let _on = TestMode::set(true);
    assert!(matches!(
        triage("libcuda.so.1()(64bit)", &|_| false),
        Triage::Skip(Skip::Driver(_))
    ));
    assert!(matches!(
        triage("libnvidia-ml.so.1()(64bit)", &|_| false),
        Triage::Skip(Skip::Driver(_))
    ));
}

#[test]
fn a_cuda_package_name_is_native_and_not_python_or_flavored() {
    let _on = TestMode::set(true);
    assert_eq!(
        build("cuda-cublas"),
        Some(("cuda-cublas".to_string(), PackageKind::Native))
    );
    assert_eq!(
        build("cuda-cublas-devel"),
        Some(("cuda-cublas".to_string(), PackageKind::Native))
    );
    assert_eq!(osc::strip_rpm_prefix("cuda-cublas"), "cuda-cublas");
    // Not a component: untouched.
    assert_eq!(
        build("cuda-toolkit"),
        Some(("cuda-toolkit".to_string(), PackageKind::Unknown))
    );
    // A queued wheel is available as the cuda package, by either spelling.
    let own: HashSet<String> = ["cuda-cublas".to_string()].into();
    for dep in ["nvidia-cublas", "cuda-cublas"] {
        assert!(matches!(
            availability(dep, &own, &HashMap::new(), &[]),
            Availability::InProject { .. }
        ));
    }
}
