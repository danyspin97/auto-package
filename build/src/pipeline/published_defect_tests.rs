use super::*;

/// python-fastapi-cloud-cli built in OBS with `Requires: x if y` and every
/// dependent was unresolvable on `nothing provides if`; the resolver has to
/// see such a package as not done.
/// python-flashinfer-python: an arch package that names CUDA and does not
/// leave the NVIDIA driver out of its Requires is rebuilt with the exclude.
#[test]
fn a_cuda_arch_package_without_the_driver_exclude_is_defective_until_it_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("python-flashinfer-python.spec");
    let text = "Name: python-flashinfer-python\nBuildRequires: cuda-nvcc\n%python_subpackages\n";
    std::fs::write(&spec, text).unwrap();
    assert!(spec_has_published_defects(&spec, Path::new("/nonexistent")));
    let fixed = spec_with_published_defects_fixed(text, dir.path(), Path::new("/nonexistent"));
    assert!(
        fixed.contains("%global __requires_exclude ^(libcuda"),
        "{fixed}"
    );
    std::fs::write(&spec, &fixed).unwrap();
    assert!(!spec_has_published_defects(
        &spec,
        Path::new("/nonexistent")
    ));
    // A noarch package cannot link the driver.
    std::fs::write(&spec, "Name: x\nBuildArch: noarch\nRequires: cuda\n").unwrap();
    assert!(!spec_has_published_defects(
        &spec,
        Path::new("/nonexistent")
    ));
}

#[test]
fn a_built_package_with_a_bare_conditional_dependency_is_defective() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("python-x.spec");
    std::fs::write(
        &spec,
        "Requires:  python-tomli >= 2 if python-base < 3.11\n",
    )
    .unwrap();
    assert!(spec_has_published_defects(&spec, Path::new("/nonexistent")));
    std::fs::write(
        &spec,
        "Requires:  (python-tomli >= 2 if python-base < 3.11)\n",
    )
    .unwrap();
    assert!(!spec_has_published_defects(
        &spec,
        Path::new("/nonexistent")
    ));
    // No spec checked out: nothing to repair.
    assert!(!spec_has_published_defects(
        &dir.path().join("absent.spec"),
        Path::new("/nonexistent")
    ));
}
