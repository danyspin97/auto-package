use super::*;

#[test]
fn pypi_name_from_canonical_source_url() {
    assert_eq!(
        pypi_name_from_source_url(
            "https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz"
        )
        .as_deref(),
        Some("attrs"),
    );
}

#[test]
fn pypi_name_rejects_github_source() {
    assert_eq!(
        pypi_name_from_source_url("https://github.com/pallets/markupsafe/archive/3.0.3.tar.gz"),
        None,
    );
}

#[test]
fn pypi_name_preserves_case() {
    assert_eq!(
        pypi_name_from_source_url(
            "https://files.pythonhosted.org/packages/source/M/MarkupSafe/MarkupSafe-%{version}.tar.gz"
        )
        .as_deref(),
        Some("MarkupSafe"),
    );
}

#[test]
fn is_newer_decimal_segments_compared_numerically() {
    assert!(is_newer_pep440("1.10.0", "1.9.0"));
    assert!(!is_newer_pep440("1.9.0", "1.10.0"));
    assert!(!is_newer_pep440("1.9.0", "1.9.0"));
}

#[test]
fn is_newer_prerelease_is_older_than_release() {
    assert!(!is_newer_pep440("2.0.0rc1", "2.0.0"));
    assert!(is_newer_pep440("2.0.0", "2.0.0rc1"));
}

#[test]
fn is_newer_refuses_on_parse_failure() {
    assert!(!is_newer_pep440("not-a-version", "1.0"));
    assert!(!is_newer_pep440("1.0", "not-a-version"));
}

#[test]
fn wheel_filename_yields_distribution_and_version() {
    assert_eq!(
        pypi_name_and_version_from_wheel_filename(
            "nvidia_cublas-13.1.1.3-py3-none-manylinux_2_27_x86_64.whl"
        ),
        Some(("nvidia_cublas".to_string(), "13.1.1.3".to_string())),
    );
}

#[test]
fn wheel_filename_keeps_local_version_qualifier() {
    // PyPI wheels can embed `+<local>` — the version segment is still just
    // the single dash-delimited token at position 1.
    assert_eq!(
        pypi_name_and_version_from_wheel_filename("torch-2.4.0+cpu-cp311-cp311-linux_x86_64.whl"),
        Some(("torch".to_string(), "2.4.0+cpu".to_string())),
    );
}

#[test]
fn wheel_filename_rejects_non_wheel() {
    assert_eq!(
        pypi_name_and_version_from_wheel_filename("attrs-26.1.0.tar.gz"),
        None,
    );
}

#[test]
fn wheel_filename_rejects_truncated_filename() {
    // Fewer than 5 hyphen-separated segments means not a real wheel name.
    assert_eq!(
        pypi_name_and_version_from_wheel_filename("foo-1.0.whl"),
        None
    );
}

#[test]
fn tarball_filename_extracts_name_from_template() {
    assert_eq!(
        pypi_name_from_versioned_tarball("torchaudio-%{version}.tar.gz", "2.4.0").as_deref(),
        Some("torchaudio"),
    );
    assert_eq!(
        pypi_name_from_versioned_tarball("nvidia-cuda-cccl-%{version}.tar.gz", "13.4.92")
            .as_deref(),
        Some("nvidia-cuda-cccl"),
    );
}

#[test]
fn tarball_filename_extracts_name_against_literal_version() {
    assert_eq!(
        pypi_name_from_versioned_tarball("cuda-core-1.2.0.tar.gz", "1.2.0").as_deref(),
        Some("cuda-core"),
    );
    assert_eq!(
        pypi_name_from_versioned_tarball("cuda-python-13.4.1.tar.gz", "13.4.1").as_deref(),
        Some("cuda-python"),
    );
}

#[test]
fn tarball_filename_literal_form_needs_the_right_version() {
    assert!(
        pypi_name_from_versioned_tarball("cuda-core-1.2.0.tar.gz", "1.3.0").is_none(),
        "spec version has to actually match the embedded one"
    );
}

#[test]
fn tarball_filename_rejects_pure_name() {
    assert!(pypi_name_from_versioned_tarball("something.tar.gz", "9.9.9").is_none());
}

#[test]
fn tarball_filename_rejects_urls_and_other_macros() {
    for src in [
        "https://github.com/o/r/releases/download/v%{version}/r-%{version}.tar.gz",
        "https://github.com/o/r/archive/foo-1.2.3.tar.gz",
        "%{name}-%{version}.tar.gz",
    ] {
        assert!(
            pypi_name_from_versioned_tarball(src, "1.2.3").is_none(),
            "{src}"
        );
    }
}
