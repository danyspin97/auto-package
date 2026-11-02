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
