//! Tests of the requires excludes of hashed libraries and the NVIDIA driver.

use super::*;

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
