use super::*;

use super::helpers::*;

#[test]
pub(super) fn dotted_numbers_compare_by_part_and_words_stay_unknown() {
    use pypi::VersionCompare::{Newer, NotNewer, Unparseable};
    assert!(matches!(
        compare_versions("2.10", "2.9", Scheme::Generic),
        Newer
    ));
    assert!(matches!(
        compare_versions("2.9", "2.10", Scheme::Generic),
        NotNewer
    ));
    assert!(matches!(
        compare_versions("059", "059", Scheme::Generic),
        NotNewer
    ));
    assert!(matches!(
        compare_versions("1.0", "%{ver}", Scheme::Generic),
        Unparseable
    ));
}

#[test]
pub(super) fn github_archive_url_is_trimmed_to_the_bare_repo() {
    assert_eq!(
        git_repo_url("https://github.com/BurntSushi/ripgrep/archive/14.1.0.tar.gz").as_deref(),
        Some("https://github.com/BurntSushi/ripgrep"),
    );
    assert_eq!(
        git_repo_url("https://github.com/pallets/markupsafe/").as_deref(),
        Some("https://github.com/pallets/markupsafe"),
    );
}

#[test]
pub(super) fn git_repo_url_rejects_non_forge_hosts() {
    assert_eq!(
        git_repo_url("https://files.pythonhosted.org/packages/source/a/attrs/attrs.tar.gz"),
        None,
    );
}

#[test]
pub(super) fn git_repo_url_preserves_nested_gitlab_groups() {
    // A nested GitLab group has to survive intact: GitLab's REST API
    // returns 404 for a truncated path, and the archive URL built from
    // it just doesn't exist. Three-segment path, three-segment URL.
    assert_eq!(
        git_repo_url(
            "https://gitlab.com/group/subgroup/project/-/archive/v1.0/project-v1.0.tar.gz",
        )
        .as_deref(),
        Some("https://gitlab.com/group/subgroup/project"),
    );
}

#[test]
pub(super) fn git_repo_url_normalises_ssh_to_https() {
    // A `Source:` line pointing at an SSH clone URL was passed through
    // unchanged — the subsequent HTTPS release-API call then couldn't
    // reach it. Now the SSH spelling is reduced to its canonical HTTPS
    // form before anything else runs.
    assert_eq!(
        git_repo_url("git@github.com:NVIDIA/cuda-python.git").as_deref(),
        Some("https://github.com/NVIDIA/cuda-python"),
    );
}

#[test]
pub(super) fn version_prefixes_and_epochs_are_compared_as_numbers() {
    use pypi::VersionCompare::{Newer, NotNewer};
    let table = [
        ("r63", "62", true),
        ("r62", "62", false),
        ("v1.3", "1.2", true),
        ("1.2.3a", "1.2.3", true),
        ("1.2.3", "1.2.3a", false),
        ("1.2.3b", "1.2.3a", true),
        ("1.10", "1.9a", true),
        ("1.2.3.4", "1.2.3", true),
        ("1.2.3.10", "1.2.3.9", true),
        ("1.2.3.4.5", "1.2.3.4", true),
        ("2:1.0", "3.0", true),
        ("3.0", "1:1.0", false),
        ("1:1.0", "1:1.1", false),
        ("1.2+git20260101", "1.2", true),
        ("1.0~rc1", "1.0", false),
        ("2.0", "1.0~git20260101", true),
    ];
    for (latest, packaged, newer) in table {
        let want = if newer { Newer } else { NotNewer };
        assert!(
            compare_versions(latest, packaged, Scheme::Generic) == want,
            "{latest} vs {packaged}"
        );
    }
}

#[test]
pub(super) fn series_helpers_accept_prefixes_and_long_versions() {
    assert_eq!(version_part("release-1.3"), "1.3");
    assert_eq!(version_part("r62"), "62");
    assert_eq!(version::leading_numbers("r62"), vec![62]);
    assert_eq!(version::leading_numbers("v1.2.3.4"), vec![1, 2, 3, 4]);
    assert_eq!(version::leading_numbers("1.2.3a"), vec![1, 2, 3]);
    assert_eq!(series_of("boost-1.85", "1.85.0"), Some(vec![1, 85]));
    assert_eq!(series_of("fmt-9", "v9.1.0"), Some(vec![9]));
    assert!(in_series("v9.2", &[9]));
    assert!(!in_series("10.0", &[9]));
    assert!(in_series("1.85.0.1", &[1, 85]));
    assert!(!in_series("1.8", &[1, 85]));
}

#[test]
pub(super) fn an_r_numbered_release_and_a_snapshot_packaged_version() {
    let spec = "Name: inih\nVersion: 62\nURL: https://github.com/benhoyt/inih\n\
                    Source0: https://github.com/benhoyt/inih/archive/r%{version}.tar.gz\n";
    let fetch = |_: &str| Ok(spec.to_string());
    let c = classify("inih", &fetch, &only_repo("r62"));
    assert!(
        matches!(c.decision, Decision::UpToDate { .. }),
        "{:?}",
        c.decision
    );
    let c = classify("inih", &fetch, &only_repo("r63"));
    assert!(
        matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "63"),
        "{:?}",
        c.decision
    );
    assert!(version::is_snapshot("1.0~git.20251221"));
    let spec = "Name: klib\nVersion: 1.0~git.20251221\nURL: https://github.com/attractivechaos/klib\n\
                    Source0: https://github.com/attractivechaos/klib/archive/master.tar.gz\n";
    let c = classify("klib", &|_: &str| Ok(spec.to_string()), &only_repo("r394"));
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::Snapshot { .. })),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_prerelease_is_never_offered_over_a_stable_release() {
    let spec = "Name: foo\nVersion: 1.3.11\nURL: https://github.com/o/foo\n\
                    Source0: https://github.com/o/foo/archive/v%{version}.tar.gz\n";
    let fetch = |_: &str| Ok(spec.to_string());
    for latest in [
        "1.3.12-pre3",
        "1.0.0-beta2",
        "2.0rc1",
        "2.0.0-alpha",
        "1.4~git20260101",
    ] {
        let c = classify("foo", &fetch, &only_repo(latest));
        assert!(
            matches!(c.decision, Decision::Skipped(SkipReason::Prerelease { .. })),
            "{latest}: {:?}",
            c.decision
        );
    }
    // A snapshot-dated tag has a `-` and is no release either.
    let c = classify("foo", &fetch, &only_repo("0.19-20260627"));
    assert!(matches!(
        c.decision,
        Decision::Skipped(SkipReason::NotAnRpmVersion { .. })
    ));
    // A package already on a pre-release may move to the next one.
    let spec = spec.replace("1.3.11", "2.0rc1");
    let c = classify("foo", &|_: &str| Ok(spec.clone()), &only_repo("2.0rc2"));
    assert!(
        matches!(c.decision, Decision::Update { .. }),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_failed_lookup_says_what_to_do_and_is_not_a_missing_update() {
    let reason = |error: &str| {
        SkipReason::LookupFailed {
            ecosystem_tag: "release listing".into(),
            name: "https://x/".into(),
            error: error.into(),
        }
        .to_string()
    };
    assert!(reason("https://x/: http status: 404").contains("looks dead"));
    assert!(reason("invalid peer certificate: Expired").contains("certificate"));
    assert!(reason("Repology has no newest version for it").contains("Repology"));
    assert!(reason("boom").contains("unknown"));
}

#[test]
pub(super) fn pypi_packages_compare_by_pep440_the_rest_generically() {
    let spec = "Name: python-x\nVersion: 1.2.3\n\
                    Source: https://files.pythonhosted.org/packages/source/x/x/x-%{version}.tar.gz\n";
    // `1.2.3a` is an alpha before 1.2.3 for PyPI.
    let c = classify(
        "python-x",
        &|_: &str| Ok(spec.to_string()),
        &only_pypi("1.2.3a"),
    );
    assert!(
        matches!(c.decision, Decision::UpToDate { .. }),
        "{:?}",
        c.decision
    );
    // An RPM-style pre-release version still compares.
    let spec = spec.replace("1.2.3", "1.0~rc1");
    let c = classify("python-x", &|_: &str| Ok(spec.clone()), &only_pypi("1.0"));
    assert!(
        matches!(c.decision, Decision::Update { .. }),
        "{:?}",
        c.decision
    );
    // The same trailing-zero version is not an update.
    let spec = "Name: python-x\nVersion: 2.0\n\
                    Source: https://files.pythonhosted.org/packages/source/x/x/x-%{version}.tar.gz\n";
    let c = classify(
        "python-x",
        &|_: &str| Ok(spec.to_string()),
        &only_pypi("2.0.0"),
    );
    assert!(
        matches!(c.decision, Decision::UpToDate { .. }),
        "{:?}",
        c.decision
    );
}

/// An obs_scm package whose `Version:` is `<tag>~0` is on the tag itself, so
/// the tag is what it is compared with; a non-zero offset is still behind the
/// tag's release and is an update.
#[test]
pub(super) fn a_zero_tag_offset_is_compared_as_the_tag() {
    let spec = |version: &str| {
        format!(
            "Name: wlrctl\nVersion: {version}\n\
             Source: https://github.com/acheronuk/wlrctl/archive/v%{{version}}.tar.gz\n"
        )
    };
    let on_tag = classify(
        "wlrctl",
        &|_: &str| Ok(spec("0.2.2~0")),
        &only_repo("0.2.2"),
    );
    assert!(
        matches!(on_tag.decision, Decision::UpToDate { .. }),
        "{:?}",
        on_tag.decision
    );
    let ahead = classify(
        "wlrctl",
        &|_: &str| Ok(spec("0.2.2~3")),
        &only_repo("0.2.2"),
    );
    assert!(
        matches!(ahead.decision, Decision::Update { .. }),
        "{:?}",
        ahead.decision
    );
}

/// An obs_scm package whose spec names no repository is looked up in the
/// repository its `_service` names.
#[test]
pub(super) fn an_obs_scm_source_is_looked_up_in_its_service_repository() {
    fn service(_: &str) -> Option<String> {
        Some(
            "<services>\n  <service name=\"obs_scm\" mode=\"manual\">\n    \
             <param name=\"url\">https://github.com/MastroHD/ashell.git</param>\n    \
             <param name=\"scm\">git</param>\n    \
             <param name=\"revision\">v0.5.0</param>\n  </service>\n</services>\n"
                .to_string(),
        )
    }
    assert_eq!(
        service_git_url(&service("").unwrap()).as_deref(),
        Some("https://github.com/MastroHD/ashell")
    );
    assert_eq!(
        service_git_url(
            "<service name=\"tar_scm\"><param name=\"url\">https://example.org/x.git</param></service>"
        ),
        None
    );
    let spec = "Name: ashell\nVersion: 0.5.0\nSource: ashell.tar.gz\n";
    let lookups = Lookups {
        fetch_service: &service,
        ..only_repo("0.6.0")
    };
    let c = classify("ashell", &|_: &str| Ok(spec.to_string()), &lookups);
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/MastroHD/ashell".into(),
            },
            old_version: "0.5.0".into(),
            new_version: "0.6.0".into(),
            upstream_tag: None,
        }
    );
}

/// A spec that downloads its source from a URL is not read for a `_service`.
#[test]
pub(super) fn a_spec_with_a_download_url_does_not_read_its_service() {
    let spec = "Name: x\nVersion: 0.5.0\nSource: https://example.org/x-%{version}.tar.gz\n";
    let lookups = Lookups {
        fetch_service: &|_: &str| -> Option<String> { panic!("the _service is not read") },
        ..unused_lookups()
    };
    let c = classify("x", &|_: &str| Ok(spec.to_string()), &lookups);
    assert!(
        !matches!(c.decision, Decision::Update { .. }),
        "{:?}",
        c.decision
    );
}

/// Only a live `obs_scm` service that fetches from git names a package's
/// repository. A commented-out, disabled, non-git or self-closing one does not,
/// and a `tar_scm` service is never read: its snapshot is not proposed for the
/// newest tag.
#[test]
pub(super) fn only_a_live_obs_scm_git_service_names_the_repository() {
    const URL: &str = "https://github.com/MastroHD/ashell";
    let cases = [
        (
            "<!-- <service name=\"obs_scm\"><param name=\"url\">URL.git</param></service> -->",
            None,
        ),
        (
            "<service name=\"obs_scm\" mode=\"disabled\"><param name=\"url\">URL.git</param></service>",
            None,
        ),
        (
            "<service name=\"obs_scm\"><param name=\"url\">URL.git</param><param name=\"scm\">svn</param></service>",
            None,
        ),
        // The self-closing service has no url; the next service's is not its own.
        (
            "<service name=\"obs_scm\" mode=\"manual\"/>\n<service name=\"download_url\"><param name=\"url\">URL.git</param></service>",
            None,
        ),
        (
            "<service name=\"tar_scm\"><param name=\"url\">URL.git</param></service>",
            None,
        ),
        (
            "<service name=\"obs_scm\"><param name=\"url\">URL.git</param></service>",
            Some(URL),
        ),
        (
            "<service name=\"obs_scm\"><param name=\"url\">URL.git</param><param name=\"scm\">git</param></service>",
            Some(URL),
        ),
    ];
    for (service, expected) in cases {
        let service = service.replace("URL", URL);
        assert_eq!(service_git_url(&service).as_deref(), expected, "{service}");
    }
    // A tar_scm snapshot is not proposed for the newest tag of its repository.
    let snapshot = "Name: ashell\nVersion: 0.5.0+git20240101\nSource: ashell.tar.gz\n";
    let tar_scm = |_: &str| {
        Some(format!(
            "<service name=\"tar_scm\"><param name=\"url\">{URL}.git</param></service>"
        ))
    };
    let lookups = Lookups {
        fetch_service: &tar_scm,
        ..only_repo("0.6.0")
    };
    let c = classify("ashell", &|_: &str| Ok(snapshot.to_string()), &lookups);
    assert!(
        !matches!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::GitRepo { .. },
                ..
            }
        ),
        "{:?}",
        c.decision
    );
}

/// A dashed tag is compared as the RPM version it maps to, and the tag travels
/// with the decision.
#[test]
pub(super) fn a_dashed_tag_is_compared_as_its_rpm_version() {
    let spec = |version: &str| {
        format!(
            "Name: cuarzo-srm\nVersion: {version}\n\
             Source: https://github.com/CuarzoSoftware/SRM/archive/refs/tags/v%{{version}}.tar.gz\n"
        )
    };
    let update = |version: &str, latest: &'static str| {
        classify(
            "cuarzo-srm",
            &|_: &str| Ok(spec(version)),
            &only_repo(latest),
        )
        .decision
    };
    assert_eq!(
        update("0.8.0", "v0.8.0-2"),
        Decision::Update {
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/CuarzoSoftware/SRM".into(),
            },
            old_version: "0.8.0".into(),
            new_version: "0.8.0.2".into(),
            upstream_tag: Some("v0.8.0-2".into()),
        }
    );
    assert!(matches!(
        update("0.8.0.1", "v0.8.0-2"),
        Decision::Update { ref new_version, .. } if new_version == "0.8.0.2"
    ));
    assert!(matches!(
        update("0.8.0.2", "v0.8.0-1"),
        Decision::UpToDate { .. }
    ));
    // A pre-release tag is no update of a stable release.
    assert!(matches!(
        update("0.1.0", "0.1.0-alpha"),
        Decision::Skipped(SkipReason::Prerelease { .. })
    ));
    // A dash that means neither is still refused, and says so.
    let Decision::Skipped(reason) = update("1.0.0", "1.0-prefix") else {
        panic!("a tag with an unknown dash must be skipped");
    };
    assert!(matches!(reason, SkipReason::NotAnRpmVersion { .. }));
    assert!(
        reason.to_string().contains("needs a maintainer"),
        "{reason}"
    );
}

/// A forge's dashed tag is carried as the tag it is, prefix and all, so a
/// define that holds it can keep its `v`.
#[test]
pub(super) fn a_forge_tag_is_carried_with_its_prefix() {
    let spec = "Name: liblouvre\nVersion: 1.0.0.1\n%define sillyver v1.0.0-1\n\
                Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/%{sillyver}.tar.gz\n";
    let c = classify(
        "liblouvre",
        &|_: &str| Ok(spec.to_string()),
        &only_repo("v1.0.0-2"),
    );
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/CuarzoSoftware/Louvre".into(),
            },
            old_version: "1.0.0.1".into(),
            new_version: "1.0.0.2".into(),
            upstream_tag: Some("v1.0.0-2".into()),
        }
    );
}

/// Repology answers with a distro's numbering: its `1.5-1` is a revision of
/// 1.5, not the RPM version `1.5.1`, so it is refused rather than mapped.
#[test]
pub(super) fn a_dash_from_repology_is_refused_not_mapped() {
    let spec = "Name: ripgrep-x\nVersion: 1.5.1\nSource: ripgrep-x-%{version}.tar.gz\n";
    let repology = |_: &str| Ok("1.5-1".to_string());
    let lookups = Lookups {
        fetch_repology_latest: &repology,
        ..unused_lookups()
    };
    let c = classify("ripgrep-x", &|_: &str| Ok(spec.to_string()), &lookups);
    assert!(
        matches!(
            c.decision,
            Decision::Skipped(SkipReason::NotAnRpmVersion { ref latest }) if latest == "1.5-1"
        ),
        "{:?}",
        c.decision
    );
}
