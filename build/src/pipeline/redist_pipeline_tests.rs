use super::*;
use crate::redist::TestMode;
use crate::redist::tests::Fake;

#[test]
fn a_generated_redist_spec_is_not_reviewed() {
    let _on = TestMode::set(true);
    assert!(!review_turn_wanted(true, false, false, "cuda-cudart"));
    assert!(review_turn_wanted(true, false, false, "python-numpy"));
    assert!(!review_turn_wanted(false, true, false, "python-numpy"));
    assert!(!review_turn_wanted(true, false, true, "python-numpy"));
    drop(_on);
    let _off = TestMode::set(false);
    assert!(review_turn_wanted(true, false, false, "cuda-cudart"));
}

fn config(pkg: &str, version: &str, arch: &str) -> PipelineConfig {
    PipelineConfig {
        pkg_name: pkg.into(),
        rpm_name: osc::rpm_name(pkg),
        version: version.into(),
        project: "p".into(),
        work_dir: PathBuf::new(),
        state_root: PathBuf::new(),
        rpms_dir: PathBuf::new(),
        distro: "openSUSE_Tumbleweed".into(),
        arch: arch.into(),
        max_attempts: 1,
        budget_usd: 1.0,
        no_agent: true,
        force: false,
        regen: false,
        source_repo: None,
        subprojects: Vec::new(),
        failed_deps: Vec::new(),
        unbuildable_deps: Vec::new(),
        run_id: "test".into(),
        source_urls: Default::default(),
        python_flavors: Default::default(),
        patch_request: None,
        patch_condition: None,
    }
}

#[test]
fn a_cuda_wheel_node_is_packaged_from_nvidias_archive_up_to_the_written_spec() {
    let _on = TestMode::set(true);
    let dir = tempfile::tempdir().unwrap();
    let top = "cuda_cudart-linux-x86_64-13.4.92-archive";
    let fake = Fake::new(top);
    // A leftover of an earlier release is dropped with the old source.
    let stale = dir
        .path()
        .join("cuda_cudart-linux-x86_64-13.3.0-archive.tar.xz");
    std::fs::write(&stale, b"old").unwrap();
    // The node `nvidia-cuda-runtime` is the package `cuda-cudart`, at the manifest's version.
    let config = config("nvidia-cuda-runtime", "13.4.92", "x86_64");
    assert_eq!(config.rpm_name, "cuda-cudart");
    let spec_path = dir.path().join("cuda-cudart.spec");
    let artifact = dir.path().join("spec.generated");

    let Ok(res) = generate_redist_spec(&config, dir.path(), &spec_path, Some(&artifact), &fake)
    else {
        panic!("the generation failed");
    };

    // No sdist, no Python flow.
    assert!(!res.is_maturin && !res.is_rust && !res.from_repo_archive);
    assert_eq!(res.sdist_top_level, "");
    let spec = std::fs::read_to_string(&spec_path).unwrap();
    assert_eq!(spec, std::fs::read_to_string(&artifact).unwrap());
    assert!(spec.contains("Name:           cuda-cudart\n"), "{spec}");
    assert!(spec.contains("Version:        13.4.92\n"));
    assert!(spec.contains("License:        SUSE-NonFree\n"));
    assert!(!spec.contains("%pyproject") && !spec.contains("%python_expand"));
    assert!(spec.contains(&format!("# Source archive sha256: {}\n", fake.advertised)));
    let rc = std::fs::read_to_string(dir.path().join("cuda-cudart.rpmlintrc")).unwrap();
    assert!(rc.contains("filelist-forbidden-fhs23"));
    assert!(dir.path().join(format!("{top}.tar.xz")).is_file());
    assert!(!stale.exists());
    // OBS gets the spec, the archive and the rpmlintrc.
    let plan = osc::publish_plan(dir.path());
    for f in [
        "cuda-cudart.spec",
        "cuda-cudart.rpmlintrc",
        &format!("{top}.tar.xz"),
    ] {
        assert!(
            plan.files.iter().any(|p| p == f),
            "{f} not in {:?}",
            plan.files
        );
    }
    // The spec's version is the config's: no version bump is seen next run.
    assert!(!spec_version_differs(&spec_path, &config.version));
}

#[test]
fn a_mismatching_archive_fails_the_generation_and_leaves_no_spec() {
    let _on = TestMode::set(true);
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new("cuda_cudart-linux-x86_64-13.4.92-archive");
    fake.advertised = "b".repeat(64);
    let spec_path = dir.path().join("cuda-cudart.spec");
    let err = generate_redist_spec(
        &config("nvidia-cuda-runtime", "13.4.92", "x86_64"),
        dir.path(),
        &spec_path,
        None,
        &fake,
    );
    assert!(matches!(err, Err(GenSpecError::Other(m)) if m.contains("refusing")));
    assert!(!spec_path.exists());
}

#[test]
fn with_the_mode_off_a_cuda_wheel_is_still_a_python_package_from_its_wheel() {
    let _off = TestMode::set(false);
    let config = config("nvidia-cublas", "13.1.1.3", "x86_64");
    assert_eq!(config.rpm_name, "python-nvidia-cublas");
    assert!(!crate::redist::is_redist_package(&config.rpm_name));
    assert!(!crate::redist::is_redist_package("cuda-cublas"));
    let wheel = "nvidia_cublas-13.1.1.3-py3-none-manylinux_2_27_x86_64.whl";
    let release = ap_ecosystem::pypi::Release {
        name: "nvidia-cublas".to_string(),
        version: "13.1.1.3".to_string(),
        summary: Some("cuBLAS".to_string()),
        homepage: None,
        license: ap_ecosystem::pypi::License::FreeText("NVIDIA Proprietary".to_string()),
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: String::new(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    };
    // The wheel skeleton is the generator for it, as before.
    let spec = crate::spec::wheel_binary_skeleton(
        &config.pkg_name,
        &config.version,
        wheel,
        &release,
        Some(true),
        None,
    );
    assert!(spec.contains("python-nvidia-cublas"), "{spec}");
    assert!(!spec.contains("cuda-cublas"), "{spec}");
}
