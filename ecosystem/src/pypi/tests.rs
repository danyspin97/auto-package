use super::*;

mod fetch;
mod names;
mod yank;

/// Trimmed from the real `https://pypi.org/pypi/markupsafe/json`: 89 files
/// reduced to two, the long description dropped. Every field kept is
/// verbatim, including the nulls — `license`, `home_page` and
/// `requires_dist` really are null on a current, well-maintained package.
const MARKUPSAFE: &str = r#"{
  "info": {
    "author": null,
    "classifiers": ["Development Status :: 5 - Production/Stable",
                    "Programming Language :: Python"],
    "home_page": null,
    "license": null,
    "license_expression": "BSD-3-Clause",
    "license_files": ["LICENSE.txt"],
    "name": "MarkupSafe",
    "project_url": "https://pypi.org/project/MarkupSafe/",
    "project_urls": {
      "Changes": "https://markupsafe.palletsprojects.com/page/changes/",
      "Documentation": "https://markupsafe.palletsprojects.com/",
      "Source": "https://github.com/pallets/markupsafe/"
    },
    "requires_dist": null,
    "requires_python": ">=3.9",
    "summary": "Safely add untrusted strings to HTML/XML markup.",
    "version": "3.0.3",
    "yanked": false
  },
  "urls": [
    {
      "digests": {"md5": "aaa", "sha256": "0123456789abcdef"},
      "filename": "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
      "packagetype": "bdist_wheel",
      "size": 21978,
      "url": "https://files.pythonhosted.org/packages/aa/markupsafe-3.0.3-py3-none.whl",
      "yanked": false
    },
    {
      "digests": {"md5": "13a73126d25afa72a1ff0daed072f5fe",
                  "sha256": "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698"},
      "filename": "markupsafe-3.0.3.tar.gz",
      "packagetype": "sdist",
      "size": 80313,
      "url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
      "yanked": false
    }
  ]
}"#;

fn markupsafe() -> Release {
    parse_release(MARKUPSAFE).expect("fixture parses")
}

#[test]
fn finds_the_sdist_among_the_wheels() {
    let r = markupsafe();
    assert_eq!(r.sdist.filename, "markupsafe-3.0.3.tar.gz");
    assert_eq!(
        r.sdist.sha256,
        "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698"
    );
    assert_eq!(r.sdist.size, 80313);
}

#[test]
fn keeps_pypis_spelling_of_the_name() {
    // `Source:` URLs are case-sensitive, so the display name has to survive
    // intact. Lowercasing belongs to RPM naming, not here.
    assert_eq!(markupsafe().name, "MarkupSafe");
}

#[test]
fn prefers_the_pep_639_expression() {
    assert_eq!(
        markupsafe().license,
        License::Spdx("BSD-3-Clause".to_string())
    );
}

#[test]
fn falls_back_through_the_url_keys() {
    // The real case: `home_page` is null and there is no `Homepage` key,
    // so `Source` wins. A naive `home_page` read yields nothing here.
    assert_eq!(
        markupsafe().homepage.as_deref(),
        Some("https://github.com/pallets/markupsafe/")
    );
}

#[test]
fn an_empty_project_url_does_not_hide_the_fallbacks() {
    // `"Homepage": ""` is unset, so the `Source` URL is the homepage, and
    // whitespace-only values are unset too.
    let info = |urls: &str| -> RawInfo {
        serde_json::from_str(&format!(
            r#"{{"name": "x", "version": "1", "home_page": null,
                "project_urls": {urls}}}"#
        ))
        .unwrap()
    };
    let empty_homepage = info(r#"{"Homepage": "", "Source": "https://github.com/example/x"}"#);
    assert_eq!(
        homepage(&empty_homepage).as_deref(),
        Some("https://github.com/example/x")
    );
    let blank_homepage = info(r#"{"Homepage": "  \t ", "Source": "https://github.com/example/x"}"#);
    assert_eq!(
        homepage(&blank_homepage).as_deref(),
        Some("https://github.com/example/x")
    );
    // A real value is trimmed, and nothing usable leaves the homepage unset.
    let padded = info(r#"{"Homepage": " https://example.org/x "}"#);
    assert_eq!(homepage(&padded).as_deref(), Some("https://example.org/x"));
    let nothing = info(r#"{"Homepage": "", "Source": ""}"#);
    assert_eq!(homepage(&nothing), None);
}

#[test]
fn a_wheel_only_release_is_an_error_not_an_empty_release() {
    let json = MARKUPSAFE.replace(
        r#""packagetype": "sdist""#,
        r#""packagetype": "bdist_wheel""#,
    );
    let err = parse_release(&json).expect_err("wheel-only must not parse");
    assert!(matches!(err, Error::NoSdist { .. }), "got {err:?}");
}

#[test]
fn picks_the_wheel_matching_the_requested_arch() {
    let wheel = pick_wheel_from_json(MARKUPSAFE, "x86_64", None).expect("a wheel exists");
    assert_eq!(
        wheel.filename,
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl"
    );
    assert_eq!(wheel.sha256, "0123456789abcdef");
}

#[test]
fn refuses_the_wrong_arch_rather_than_falling_back_to_whatever_wheel_exists() {
    // The fixture only ships an x86_64 wheel. An aarch64 request used to
    // silently return that x86_64 wheel ("falls back to the only wheel")
    // and let the spec-generation code paper over the mismatch, which
    // the Oct 6 review probed as `py3-none-win_amd64` being accepted for
    // an aarch64 target. Now the pick fails outright so the agent can
    // report "no usable upstream wheel" instead of generating a broken
    // package.
    let err = pick_wheel_from_json(MARKUPSAFE, "aarch64", None)
        .expect_err("the only wheel is x86_64; aarch64 must refuse it");
    assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
}

#[test]
fn refuses_a_wheel_whose_platform_tag_is_not_linux() {
    // The win_amd64 case from the Oct 6 review: a `py3-none-win_amd64`
    // wheel passed every filter when the project arch was aarch64 (or
    // anything non-win), because the previous fallback ended at
    // `wheels().next()` with no platform validation at all.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-py3-none-win_amd64.whl",
    );
    assert!(matches!(
        pick_wheel_from_json(&json, "x86_64", None),
        Err(Error::NoWheel { .. }),
    ));
    assert!(matches!(
        pick_wheel_from_json(&json, "aarch64", None),
        Err(Error::NoWheel { .. }),
    ));
}

#[test]
fn refuses_a_wheel_whose_python_tag_is_python_2() {
    // The `py2-none-any` case from the Oct 6 review: the ABI check
    // alone said "none, OK" and the universal `any` platform passed,
    // but the wheel's Python tag was py2 and no openSUSE spec this
    // project writes builds for Python 2.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-py2-none-any.whl",
    );
    assert!(matches!(
        pick_wheel_from_json(&json, "x86_64", None),
        Err(Error::NoWheel { .. }),
    ));
    // The locked-flavor variant must also reject it — the lock covers
    // the ABI, not the language level.
    assert!(matches!(
        pick_wheel_from_json(&json, "x86_64", Some("cp313")),
        Err(Error::NoWheel { .. }),
    ));
}

#[test]
fn accepts_a_compressed_platform_tag_covering_our_arch() {
    // PEP 425 allows a wheel to list several compatible platforms in
    // one `.`-separated tag. A `manylinux1_x86_64.manylinux_2_17_x86_64`
    // wheel is installable on x86_64 even though neither segment is the
    // exact tag osc/zypper would print.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-py3-none-manylinux1_x86_64.manylinux_2_17_x86_64.whl",
    );
    let wheel = pick_wheel_from_json(&json, "x86_64", None).expect("compressed tag OK");
    assert!(wheel.filename.contains("manylinux"));
}

#[test]
fn accepts_an_abi3_wheel_regardless_of_its_cp_tag() {
    // A stable-ABI wheel tagged `cp310-abi3-...` installs under every
    // CPython 3.x, so no single-flavor lock is needed.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-cp310-abi3-manylinux_2_17_x86_64.whl",
    );
    let wheel = pick_wheel_from_json(&json, "x86_64", None).expect("abi3 is universal");
    assert!(wheel.filename.contains("abi3"));
}

#[test]
fn flavor_agnostic_check_reads_the_abi_tag_not_the_platform_tag() {
    assert!(wheel_is_flavor_agnostic(
        "nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
        None
    ));
    assert!(wheel_is_flavor_agnostic(
        "somepkg-1.0-py3-none-any.whl",
        None
    ));
    assert!(!wheel_is_flavor_agnostic(
        "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
        None
    ));
    assert!(!wheel_is_flavor_agnostic(
        "triton-3.8.0-cp310-cp310-manylinux_2_27_x86_64.whl",
        None
    ));
}

#[test]
fn refuses_a_wheel_locked_to_one_cpython_abi() {
    // Triton's real shape: genuine compiled extensions, published per
    // CPython version (cp310, cp311, ...) with no universal build. A
    // single such wheel cannot satisfy every Python flavor an openSUSE
    // spec builds for (python313 *and* python314 side by side) — picking
    // one and repackaging it as if it were universal would silently
    // break every flavor except the one it was built for, so this must
    // report no usable wheel rather than pick an arbitrary one.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
    );
    let err =
        pick_wheel_from_json(&json, "x86_64", None).expect_err("cp-locked wheel must be refused");
    assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
}

#[test]
fn a_locked_wheel_matching_the_single_configured_flavor_is_accepted() {
    // A project configured (via flavors.toml) to build for exactly one
    // Python flavor has no second flavor left for a cp-locked wheel to be
    // broken under, so the one wheel matching that exact ABI is safe to
    // repackage automatically.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl",
    );
    let wheel = pick_wheel_from_json(&json, "x86_64", Some("cp314"))
        .expect("the single configured flavor's own wheel must be accepted");
    assert_eq!(
        wheel.filename,
        "markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl"
    );
}

/// Triton's shape: one wheel per CPython, none universal. Each flavor gets
/// its own; a flavor with no wheel fails the whole pick.
#[test]
fn one_locked_wheel_per_flavor_is_picked_and_a_gap_fails_the_set() {
    let mut v: serde_json::Value = serde_json::from_str(MARKUPSAFE).unwrap();
    let first = v["urls"][0].clone();
    let mk = |name: &str| {
        let mut u = first.clone();
        u["filename"] = name.into();
        u["url"] = format!("https://files.example/{name}").into();
        u
    };
    v["urls"] = serde_json::Value::Array(vec![
        mk("markupsafe-3.0.3-cp313-cp313-manylinux_2_5_x86_64.whl"),
        mk("markupsafe-3.0.3-cp314-cp314-manylinux_2_5_x86_64.whl"),
    ]);
    let json = v.to_string();
    let abis = vec!["cp313".to_string(), "cp314".to_string()];
    let picked = pick_wheels_per_flavor(&json, "x86_64", &abis).unwrap();
    assert_eq!(picked[0].0, "cp313");
    assert!(picked[0].1.filename.contains("cp313-cp313"));
    assert!(picked[1].1.filename.contains("cp314-cp314"));

    let with_gap = vec!["cp313".to_string(), "cp312".to_string()];
    assert!(pick_wheels_per_flavor(&json, "x86_64", &with_gap).is_err());
}

#[test]
fn a_locked_wheel_not_matching_the_single_configured_flavor_is_still_refused() {
    // Configured for cp314 but the only wheel on offer is cp313 — still
    // broken for the one flavor this project actually builds.
    let json = MARKUPSAFE.replace(
        "markupsafe-3.0.3-py3-none-manylinux_2_5_x86_64.whl",
        "markupsafe-3.0.3-cp313-cp313-manylinux_2_5_x86_64.whl",
    );
    let err = pick_wheel_from_json(&json, "x86_64", Some("cp314"))
        .expect_err("a wheel for the wrong flavor must still be refused");
    assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
}

#[test]
fn no_wheel_at_all_is_an_error() {
    let json = MARKUPSAFE.replace(
        r#""packagetype": "bdist_wheel""#,
        r#""packagetype": "sdist""#,
    );
    let err = pick_wheel_from_json(&json, "x86_64", None).expect_err("no wheel in this fixture");
    assert!(matches!(err, Error::NoWheel { .. }), "got {err:?}");
}

#[test]
fn legacy_free_text_license_is_kept_but_marked_unnormalised() {
    let json = MARKUPSAFE
        .replace(
            r#""license_expression": "BSD-3-Clause""#,
            r#""license_expression": null"#,
        )
        .replace(r#""license": null"#, r#""license": "BSD License""#);
    assert_eq!(
        parse_release(&json).unwrap().license,
        License::FreeText("BSD License".to_string())
    );
}

#[test]
fn classifiers_are_the_last_licence_resort() {
    let json = MARKUPSAFE
        .replace(
            r#""license_expression": "BSD-3-Clause""#,
            r#""license_expression": null"#,
        )
        .replace(
            r#""Programming Language :: Python""#,
            r#""License :: OSI Approved :: BSD License""#,
        );
    assert_eq!(
        parse_release(&json).unwrap().license,
        License::Classifiers(vec!["License :: OSI Approved :: BSD License".to_string()])
    );
}

#[test]
fn no_licence_information_at_all_is_reported_not_guessed() {
    let json = MARKUPSAFE.replace(
        r#""license_expression": "BSD-3-Clause""#,
        r#""license_expression": null"#,
    );
    assert_eq!(parse_release(&json).unwrap().license, License::Unknown);
}

#[test]
fn empty_strings_count_as_absent() {
    // Old records use "" and "UNKNOWN" where new ones use null.
    let json = MARKUPSAFE
        .replace(r#""home_page": null"#, r#""home_page": "UNKNOWN""#)
        .replace(
            r#""license_expression": "BSD-3-Clause""#,
            r#""license_expression": "   ""#,
        );
    let r = parse_release(&json).unwrap();
    assert_eq!(r.license, License::Unknown);
    // …and the URL chain still finds Source rather than stopping at UNKNOWN.
    assert_eq!(
        r.homepage.as_deref(),
        Some("https://github.com/pallets/markupsafe/")
    );
}
