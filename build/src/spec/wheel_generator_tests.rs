
use super::*;
use ap_ecosystem::sdist::WheelLayout;

fn release(license: License) -> Release {
    Release {
        name: "pkg".to_string(),
        version: "1.0".to_string(),
        summary: Some("A package".to_string()),
        homepage: Some("https://example.org".to_string()),
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

fn skeleton(layout: &WheelLayout, license: License) -> String {
    wheel_binary_skeleton(
        "pkg",
        "1.0",
        "pkg-1.0-py3-none-any.whl",
        &release(license),
        Some(true),
        Some(layout),
    )
}

#[test]
fn the_skeleton_never_mentions_conventions_or_a_missing_sdist() {
    let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
    assert!(!spec.contains("convention"), "{spec}");
    assert!(!spec.contains("no sdist"), "{spec}");
    assert!(!spec.contains("Confirm the arch"), "{spec}");
    let desc = spec.split("%description\n").nth(1).unwrap();
    let desc = desc.split("%prep").next().unwrap();
    assert!(!desc.contains('#') && !desc.contains("SOURCE0"), "{desc}");
}

#[test]
fn the_no_sdist_note_goes_under_prep_once() {
    let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
    let noted = with_no_sdist_note(&spec);
    let (before, after) = noted.split_once("%prep\n").unwrap();
    assert!(!before.contains("no sdist"));
    assert!(after.starts_with("# this release has no sdist and no source repository"));
    assert_eq!(noted.matches("no sdist").count(), 1);
    assert!(!noted.contains("convention"));
}

#[test]
fn header_cleanups_share_one_comment() {
    let layout = WheelLayout {
        importable: Some("pkg".into()),
        files: vec![
            "pkg/__init__.py".into(),
            "pkg/inc/a.h".into(),
            "pkg/other/b.h".into(),
            "pkg/other/b.py".into(),
        ],
        header_dirs: vec!["pkg/inc".into(), "pkg/other".into()],
        dist_info: Some("pkg-1.0.dist-info".into()),
        ..Default::default()
    };
    let spec = skeleton(&layout, License::Spdx("MIT".into()));
    assert_eq!(
        spec.matches("bundled headers are not needed").count(),
        1,
        "{spec}"
    );
    assert!(spec.contains("rm -rf"), "{spec}");
    assert!(spec.contains("-name '*.h'"), "{spec}");
}

#[test]
fn the_description_comes_from_the_metadata_paragraph() {
    let layout = WheelLayout {
        description: Some(
            "Pkg does 100% of the work for you and it is a very long sentence that \
                 has to be wrapped over more than one line to stay readable."
                .into(),
        ),
        ..Default::default()
    };
    let spec = skeleton(&layout, License::Spdx("MIT".into()));
    let desc = spec.split("%description\n").nth(1).unwrap();
    let desc = desc.split("\n\n").next().unwrap();
    assert!(desc.contains("100%% of the work"), "{desc}");
    assert!(
        desc.lines().count() >= 2 && desc.lines().all(|l| l.len() <= 80),
        "{desc}"
    );
    assert!(description_problems(&spec).is_empty(), "{spec}");
    // Without a paragraph the summary stays.
    let plain = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
    assert!(plain.contains("%description\nA package\n"), "{plain}");
}

#[test]
fn the_licence_is_mapped_and_never_pasted_text() {
    let layout = WheelLayout {
        license_expression: Some("LicenseRef-NVIDIA-Proprietary".into()),
        ..Default::default()
    };
    let spec = skeleton(&layout, License::Unknown);
    assert!(spec.contains("License:        SUSE-NonFree\n"), "{spec}");
    // PyPI's `license` field holding the whole text of the licence.
    let pasted = License::FreeText("Permission is hereby granted,\nfree of charge...".into());
    let layout = WheelLayout {
        license_expression: Some("MIT".into()),
        ..Default::default()
    };
    let spec = skeleton(&layout, pasted.clone());
    assert!(spec.contains("License:        MIT\n"), "{spec}");
    let spec = skeleton(&WheelLayout::default(), pasted);
    assert!(spec.contains("License:        FIXME\n"), "{spec}");
    let spec = skeleton(
        &WheelLayout::default(),
        License::FreeText("Apache 2.0".into()),
    );
    assert!(spec.contains("License:        Apache-2.0\n"), "{spec}");
}

#[test]
fn several_licence_files_are_one_licence_entry() {
    let layout = WheelLayout {
        importable: Some("pkg".into()),
        files: vec![
            "pkg/__init__.py".into(),
            "pkg-1.0.dist-info/METADATA".into(),
            "pkg-1.0.dist-info/licenses/LICENSE".into(),
            "pkg-1.0.dist-info/licenses/NOTICE".into(),
        ],
        dist_info: Some("pkg-1.0.dist-info".into()),
        ..Default::default()
    };
    let spec = skeleton(&layout, License::Spdx("MIT".into()));
    assert!(
        spec.contains("%license %{python_sitelib}/pkg-1.0.dist-info/licenses\n"),
        "{spec}"
    );
    assert!(
        spec.contains("%exclude %{python_sitelib}/pkg-1.0.dist-info/licenses\n"),
        "{spec}"
    );
    assert_eq!(spec.matches("%license").count(), 1, "{spec}");
    // One file keeps its own entry.
    let one = WheelLayout {
        files: layout.files[..3].to_vec(),
        ..layout
    };
    let spec = skeleton(&one, License::Spdx("MIT".into()));
    assert!(
        spec.contains("%license %{python_sitelib}/pkg-1.0.dist-info/licenses/LICENSE\n"),
        "{spec}"
    );
}

#[test]
fn bundled_vendor_binaries_add_their_licence_terms() {
    let layout = |files: &[&str]| WheelLayout {
        files: files.iter().map(|f| f.to_string()).collect(),
        ..Default::default()
    };
    let triton = layout(&["triton/backends/nvidia/bin/ptxas", "triton/__init__.py"]);
    let spec = skeleton(&triton, License::Spdx("MIT".into()));
    assert!(
        spec.contains("License:        MIT AND SUSE-NonFree\n"),
        "{spec}"
    );
    let torch = layout(&[
        "torch/lib/libgomp-a34b3233.so.1",
        "torch/lib/libcupti.so.13",
    ]);
    let spec = skeleton(&torch, License::Spdx("BSD-3-Clause OR MIT".into()));
    assert!(
            spec.contains(
                "License:        (BSD-3-Clause OR MIT) AND SUSE-NonFree AND GPL-3.0-or-later WITH GCC-exception-3.1\n"
            ),
            "{spec}"
        );
    // Already non-free: not repeated. Unknown licence: left for a person.
    let spec = skeleton(&triton, License::Spdx("SUSE-NonFree".into()));
    assert!(spec.contains("License:        SUSE-NonFree\n"), "{spec}");
    let spec = skeleton(&triton, License::Unknown);
    assert!(spec.contains("License:        FIXME\n"), "{spec}");
    // A dist-info file named like a vendor binary does not count.
    let plain = layout(&["pkg-1.0.dist-info/ptxas"]);
    assert!(wheel_vendor_license_terms(&plain).is_empty());
}

#[test]
fn a_required_extra_adds_the_packages_it_needs() {
    let deps = runtime_dependencies(
        &[
            "psycopg[binary]>=3.2".to_string(),
            "httpx[brotli,socks]".to_string(),
            "foo[unknown]".to_string(),
            "ext; extra == 'x'".to_string(),
            "uvloop; sys_platform != \"win32\"".to_string(),
            "pywin; sys_platform == \"win32\"".to_string(),
        ],
        "own",
    );
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "psycopg",
            "psycopg-binary",
            "httpx",
            "brotli",
            "socksio",
            "foo",
            "uvloop"
        ],
        "{names:?}"
    );
    assert_eq!(deps[0].bound.as_deref(), Some(">= 3.2"));
    let spec = add_runtime_dependencies(
        "Name: x\n%python_subpackages\n",
        &["psycopg[binary]".to_string()],
        "own",
    );
    assert!(
        spec.contains("Requires:       python-psycopg-binary\n"),
        "{spec}"
    );
}

#[test]
fn the_rpmlintrc_path_filters_start_with_a_wildcard_and_it_is_never_empty() {
    let layout = WheelLayout {
        first_shared_lib: Some("p/lib.so".into()),
        origin_runpath: true,
        empty_files: vec!["p/custom.css".into()],
        ..Default::default()
    };
    let (_, text) = wheel_rpmlintrc(
        "p",
        "p-1-cp313-cp313-manylinux_2_28_x86_64.whl",
        Some(&layout),
    )
    .unwrap();
    for line in text
        .lines()
        .filter(|l| l.contains("zero-length") || l.contains("rpath"))
    {
        assert!(
            line.starts_with("addFilter(r\".*") || line.starts_with("addFilter(\".*"),
            "{line}"
        );
    }
    // Nothing to filter: no file at all, never an empty one.
    let none = WheelLayout::default();
    assert!(wheel_rpmlintrc("p", "p-1-py3-none-any.whl", Some(&none)).is_none());
    assert!(wheel_rpmlintrc("p", "p-1-py3-none-any.whl", None).is_none());
}

#[test]
fn pip_ignores_requires_python_when_a_flavor_is_above_the_cap() {
    let flavors: Vec<String> = ["python311", "python313", "python314t"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(requires_python_caps_below(">=3.9,<3.13", &flavors));
    assert!(requires_python_caps_below("<=3.12", &flavors));
    assert!(requires_python_caps_below("==3.11.*", &flavors));
    assert!(!requires_python_caps_below(">=3.9", &flavors));
    assert!(!requires_python_caps_below(">=3.9,<4", &flavors));
    assert!(!requires_python_caps_below("<3.15", &flavors));
    assert!(!requires_python_caps_below("<3.12", &flavors[..1]));
    let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
    let out = with_pip_ignore_requires_python(&spec, Some("<3.12"), &flavors);
    assert_eq!(
        out.matches("export PIP_IGNORE_REQUIRES_PYTHON=1").count(),
        2,
        "{out}"
    );
    assert!(
        out.contains("%build\n# the wheel caps Requires-Python"),
        "{out}"
    );
    assert!(out.contains("%install\n# the wheel caps"), "{out}");
    // Idempotent, and absent without a cap.
    assert_eq!(
        with_pip_ignore_requires_python(&out, Some("<3.12"), &flavors),
        out
    );
    assert_eq!(
        with_pip_ignore_requires_python(&spec, Some(">=3.9"), &flavors),
        spec
    );
    assert_eq!(with_pip_ignore_requires_python(&spec, None, &flavors), spec);
}

#[test]
fn the_skeleton_keeps_requires_python_base() {
    // `%python_subpackages` does not add it: rpmlint's no-dependency-on python-base.
    let spec = skeleton(&WheelLayout::default(), License::Spdx("MIT".into()));
    assert!(spec.contains("\nRequires:       python-base\n"), "{spec}");
}
