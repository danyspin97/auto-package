use super::*;

use super::helpers::*;

#[test]
pub(super) fn flags_a_newer_upstream_release_for_update() {
    let spec = |_: &str| Ok(ATTRS_SPEC.to_string());
    let c = classify("python-attrs", &spec, &only_pypi("27.0.0"));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "attrs".into()
            },
            old_version: "26.1.0".into(),
            new_version: "27.0.0".into(),
        }
    );
}

#[test]
pub(super) fn reports_up_to_date_when_versions_match() {
    let spec = |_: &str| Ok(ATTRS_SPEC.to_string());
    let c = classify("python-attrs", &spec, &only_pypi("26.1.0"));
    assert_eq!(
        c.decision,
        Decision::UpToDate {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "attrs".into()
            },
            version: "26.1.0".into(),
        }
    );
}

#[test]
pub(super) fn reports_up_to_date_when_packaged_is_newer_than_pypi() {
    let spec = |_: &str| Ok(ATTRS_SPEC.to_string());
    let c = classify("python-attrs", &spec, &only_pypi("25.0.0"));
    assert!(matches!(c.decision, Decision::UpToDate { .. }));
}

#[test]
pub(super) fn wheel_only_source_resolves_through_pypi() {
    let spec = |_: &str| Ok(NVIDIA_CUBLAS_SPEC.to_string());
    let c = classify("python-nvidia-cublas", &spec, &only_pypi("13.2.0.0"));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "nvidia_cublas".into()
            },
            old_version: "13.1.1.3".into(),
            new_version: "13.2.0.0".into(),
        }
    );
}

#[test]
pub(super) fn a_version_an_rpm_cannot_hold_is_skipped_and_a_release_suffix_is_dropped() {
    let spec = "Name: foo\nVersion: 1.0\nURL: https://github.com/o/foo\n\
                    Source0: https://github.com/o/foo/archive/v%{version}.tar.gz\n";
    let fetch = |_: &str| Ok(spec.to_string());
    let c = classify("foo", &fetch, &only_repo("v25.12.19-2026-02-06-03fffb2"));
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::NotAnRpmVersion {
            latest: "25.12.19-2026-02-06-03fffb2".into()
        })
    );
    let c = classify("foo", &fetch, &only_repo("2.1.13-stable"));
    assert!(
        matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "2.1.13"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn declared_upstream_ignores_a_pypi_project_that_only_shares_the_name() {
    let spec = "Name: aws-c-http\nVersion: 1.1.1\nURL: https://github.com/awslabs/aws-c-http\n\
                    Source0: https://github.com/awslabs/%{name}/archive/v%{version}.tar.gz\n";
    let header = spec::parse_header(spec).unwrap();
    assert_eq!(
        declared_upstream(&header),
        DeclaredUpstream::Git("https://github.com/awslabs/aws-c-http".into())
    );
    let bare =
        spec::parse_header("Name: foo\nVersion: 1\nSource0: foo-%{version}.tar.gz\n").unwrap();
    assert_eq!(declared_upstream(&bare), DeclaredUpstream::Unknown);
}

#[test]
pub(super) fn is_newer_refuses_a_downgrade() {
    assert!(is_newer("1.1.1", "0.11.0"));
    assert!(!is_newer("0.0.1", "1.1.1"));
    assert!(!is_newer("1.1.1", "1.1.1"));
}

#[test]
pub(super) fn bare_tarball_template_resolves_through_pypi_speculatively() {
    let spec = |_: &str| Ok(TORCHAUDIO_SPEC.to_string());
    let c = classify("python-torchaudio", &spec, &only_pypi("2.5.0"));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "torchaudio".into()
            },
            old_version: "2.4.0".into(),
            new_version: "2.5.0".into(),
        }
    );
}

#[test]
pub(super) fn github_source_resolves_through_the_git_forge_api() {
    let spec = |_: &str| Ok(RIPGREP_SPEC.to_string());
    let c = classify("ripgrep", &spec, &only_repo("14.2.0"));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/BurntSushi/ripgrep".into(),
            },
            old_version: "14.1.0".into(),
            new_version: "14.2.0".into(),
        }
    );
}

#[test]
pub(super) fn skips_when_osc_cat_returns_nothing() {
    let spec = |_: &str| Err(ReadError::Failed("osc cat".into()));
    let c = classify("gone", &spec, &only_pypi("x"));
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::ReadFailed("osc cat".into()))
    );
    let spec = |_: &str| Err(ReadError::NoSpec);
    let c = classify("gone", &spec, &only_pypi("x"));
    assert_eq!(c.decision, Decision::Skipped(SkipReason::NoSpec));
}

#[test]
pub(super) fn skips_when_pypi_lookup_errors() {
    let spec = |_: &str| Ok(ATTRS_SPEC.to_string());
    let p: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(|_: &str| Err("404 Not Found".to_string())));
    let r: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(|_: &str| -> Result<String, String> {
            unreachable!()
        }));
    let lookups = Lookups {
        fetch_pypi_latest: p,
        fetch_repo_latest: r,
        ..unused_lookups()
    };
    let c = classify("python-attrs", &spec, &lookups);
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::LookupFailed {
            ecosystem_tag: "PyPI".into(),
            name: "attrs".into(),
            error: "404 Not Found".into(),
        })
    );
}

#[test]
pub(super) fn skips_when_source_is_unrecognised() {
    let spec_text = "\
Name:           weird
Version:        1.0
Source:         file:///opt/thing.tar.gz
";
    let spec = |_: &str| Ok(spec_text.to_string());
    let c = classify("weird", &spec, &only_pypi("x"));
    // Nothing in the spec says where the source lives, so only Repology
    // is left to ask.
    assert!(matches!(
        c.decision,
        Decision::Skipped(SkipReason::LookupFailed { ref ecosystem_tag, .. })
            if ecosystem_tag == "Repology"
    ));
}

#[test]
pub(super) fn a_source_url_with_macros_is_expanded_and_its_listing_read() {
    let spec_text = "\
Name:           acl
Version:        2.3.1
Url:            https://savannah.nongnu.org/projects/acl
Source:         https://download.savannah.nongnu.org/releases/acl/%{name}-%{version}.tar.xz#/acl.tar.xz
";
    let spec = |_: &str| Ok(spec_text.to_string());
    let index: &'static dyn Fn(&str) -> Result<String, String> = &|url: &str| {
        assert_eq!(
            url,
            "https://download.savannah.nongnu.org/releases/acl/acl-2.3.1.tar.xz"
        );
        Ok("2.3.2".to_string())
    };
    let c = classify("acl", &spec, &lookups_for(index, &|_| Err("no".into())));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Index {
                url: "https://download.savannah.nongnu.org/releases/acl/acl-2.3.1.tar.xz".into()
            },
            old_version: "2.3.1".into(),
            new_version: "2.3.2".into(),
        }
    );
    assert!(matches!(&c.decision, Decision::Update { ecosystem, .. } if ecosystem.can_build()));
}

#[test]
pub(super) fn a_series_package_stays_on_its_series() {
    let spec_text = "Name: fmt-9\nVersion: 9.1.0\n";
    let spec = |_: &str| Ok(spec_text.to_string());
    let newest = |v: &'static str| -> &'static dyn Fn(&str) -> Result<String, String> {
        Box::leak(Box::new(move |_: &str| Ok(v.to_string())))
    };
    let c = classify(
        "fmt-9",
        &spec,
        &lookups_for(&|_| Err("no".into()), newest("11.0.2")),
    );
    assert!(matches!(
        c.decision,
        Decision::Skipped(SkipReason::OtherSeries { ref series, .. }) if series == "9"
    ));
    let c = classify(
        "fmt-9",
        &spec,
        &lookups_for(&|_| Err("no".into()), newest("9.1.1")),
    );
    assert!(matches!(c.decision, Decision::Update { .. }));
}

#[test]
pub(super) fn a_number_in_a_name_pins_only_when_the_version_starts_with_it() {
    assert_eq!(series_of("fmt-9", "9.1.0"), Some(vec![9]));
    assert_eq!(series_of("boost-1.85", "1.85.0"), Some(vec![1, 85]));
    assert_eq!(series_of("foo-2", "1.4"), None);
    assert_eq!(series_of("python-3to2", "1.1"), None);
    assert_eq!(series_of("fmt", "11.0"), None);
}

#[test]
pub(super) fn a_package_without_a_source_line_is_asked_of_repology() {
    let spec_text = "Name: fillup\nVersion: 1.42\n";
    let spec = |_: &str| Ok(spec_text.to_string());
    let repology: &'static dyn Fn(&str) -> Result<String, String> = &|name: &str| {
        assert_eq!(name, "fillup");
        Ok("1.43".to_string())
    };
    let c = classify(
        "fillup",
        &spec,
        &lookups_for(&|_| Err("no".into()), repology),
    );
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Repology {
                project: "fillup".into()
            },
            old_version: "1.42".into(),
            new_version: "1.43".into(),
        }
    );
}

#[test]
pub(super) fn a_wrong_pypi_guess_moves_on_instead_of_skipping() {
    // `dracut-%{version}.tar.xz` looks like a PyPI project name to the
    // name-based guess, which PyPI does not know.
    let spec_text = "Name: dracut\nVersion: 059\nSource: dracut-%{version}.tar.xz\n";
    let spec = |_: &str| Ok(spec_text.to_string());
    let pypi: &'static dyn Fn(&str) -> Result<String, String> =
        &|_: &str| Err("404 Not Found".to_string());
    let lookups = Lookups {
        fetch_pypi_latest: pypi,
        fetch_repology_latest: &|_: &str| Ok("060".to_string()),
        ..unused_lookups()
    };
    let c = classify("dracut", &spec, &lookups);
    assert!(matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "060"));
}

#[test]
pub(super) fn a_project_name_in_front_of_a_tag_version_is_dropped() {
    assert_eq!(version_part("accel-config-v4.1.9"), "4.1.9");
    assert_eq!(version_part("release/2.3"), "2.3");
    assert_eq!(version_part("v1.2.3"), "1.2.3");
    assert_eq!(version_part("1.2.3"), "1.2.3");
    assert_eq!(version_part("not-a-version"), "not-a-version");
    assert_eq!(version_part("foo-2.0-rc1"), "2.0-rc1");
}

#[test]
pub(super) fn a_repository_snapshot_is_not_judged_by_a_release() {
    assert!(version::is_snapshot("2+git20260727.241e0ff"));
    assert!(version::is_snapshot(
        "84.87+git20260924.144354a1%{git_version}"
    ));
    assert!(version::is_snapshot("1.0~svn4411"));
    assert!(!version::is_snapshot("1.2.3"));
    assert!(!version::is_snapshot("1.0+gitlab")); // not a date or number after
}

#[test]
pub(super) fn a_version_finished_by_a_script_is_compared_on_what_is_known() {
    assert_eq!(
        computed_prefix("5.3.%(bash %{_sourcedir}/get_version_number.sh %{_sourcedir})").as_deref(),
        Some("5.3")
    );
    assert_eq!(computed_prefix("1.2"), None);
    assert_eq!(computed_prefix("%(date)"), None);
    assert!(!newer_than_known("5.3p20", "5.3"));
    assert!(newer_than_known("5.4", "5.3"));
    assert!(newer_than_known("6.0p1", "5.3"));
    assert!(
        !newer_than_known("5", "5.3"),
        "upstream says less than is known"
    );
}
