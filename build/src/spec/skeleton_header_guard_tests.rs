use super::*;

fn release() -> Release {
    Release {
        name: "x".into(),
        version: "1".into(),
        summary: Some("A tool".into()),
        homepage: None,
        license: License::Spdx("MIT".into()),
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

#[test]
fn a_plain_version_and_wheel_name_still_make_a_skeleton() {
    let wheel = "x-1.0.post1+local-py3-none-any.whl";
    let spec = wheel_binary_skeleton("x", "1.0.post1+local", wheel, &release(), Some(true), None);
    assert!(
        spec.contains(&format!("Source0:        {wheel}\n")),
        "{spec}"
    );
    assert!(spec.contains("Version:        1.0.post1+local\n"), "{spec}");
}

#[test]
fn a_version_that_would_rewrite_the_spec_is_refused() {
    let wheel = "x-1-py3-none-any.whl";
    for bad in ["1.0\nRequires: evil", "1.0 %{evil}", "1.0\"; rm -rf /"] {
        let spec = wheel_binary_skeleton("x", bad, wheel, &release(), Some(true), None);
        assert!(spec.is_empty(), "{bad:?} produced {spec}");
        let mono = monorepo_skeleton("x", bad, &[], Some(&release()));
        assert!(mono.is_empty(), "{bad:?} produced {mono}");
    }
}

#[test]
fn a_wheel_filename_or_name_that_would_rewrite_the_spec_is_refused() {
    let r = release();
    let spec = wheel_binary_skeleton("x", "1", "x 1.whl\n%{evil}", &r, Some(true), None);
    assert!(spec.is_empty(), "{spec}");
    let spec = wheel_binary_skeleton(
        "x\n%{evil}",
        "1",
        "x-1-py3-none-any.whl",
        &r,
        Some(true),
        None,
    );
    assert!(spec.is_empty(), "{spec}");
}

fn starts_with_header(spec: &str, package: &str) {
    let want = crate::spec_tidy::header::spec_header(package, 2041);
    assert!(spec.starts_with(&want), "{spec}");
}

#[test]
fn every_skeleton_starts_with_the_header_of_the_clock_year() {
    crate::spec_tidy::header::with_year(2041, || {
        let wheel = "x-1-py3-none-any.whl";
        starts_with_header(
            &wheel_binary_skeleton("x", "1", wheel, &release(), Some(true), None),
            "python-x",
        );
        starts_with_header(
            &monorepo_skeleton("x", "1", &[], Some(&release())),
            "python-x",
        );
        starts_with_header(
            &c_project_skeleton(
                "x",
                "1",
                "https://e.org/x",
                "x-1",
                CBuildSystem::Cmake,
                None,
            ),
            "x",
        );
        starts_with_header(
            &crate::nvidia_spec::redist_filesystem_spec("13.4.3", "1"),
            "cuda-13-4-filesystem",
        );
    });
}
