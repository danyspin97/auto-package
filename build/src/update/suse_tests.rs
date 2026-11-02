use super::*;

use super::helpers::*;

/// The `openSUSE-repos` spec as `osc cat Base:System` shows it: a GitHub
/// repository of openSUSE's, and a `Source:` with no URL of its own.
const OPENSUSE_REPOS_SPEC: &str = "\
Name:           openSUSE-repos
Version:        20260423.1a6a0f3
URL:            https://github.com/openSUSE/openSUSE-repos
Source:         openSUSE-repos-%{version}.tar.xz
";

/// `branding-upstream` as `osc cat Base:System` shows it: no URL at all.
const BRANDING_UPSTREAM_SPEC: &str = "\
Name:           branding-upstream
Version:        16.0
Source:         %{name}-COPYING
";

/// `filesystem` as `osc cat Base:System` shows it: a `_link`, a computed
/// version and a `Source:` with no URL.
const FILESYSTEM_SPEC: &str = "\
Name:           filesystem
Version:        %(echo %suse_version | cut -b-2).%(echo %suse_version | cut -b3)
URL:            https://build.opensuse.org/package/show/openSUSE:Factory/filesystem
Source0:        directory.list
";

/// `ca-certificates-cacert` as `osc cat Base:System` shows it: its `Source:`
/// is a URL that no ecosystem can read a release from.
const CACERT_SPEC: &str = "\
Name:           ca-certificates-cacert
Version:        1
URL:            https://www.cacert.org/
Source:         http://www.cacert.org/certs/CAcert_Class3Root_x14E228.crt#/CAcert_class3.pem
";

const NO_RELEASE_ANSWER: &str = "could not work out the latest version of {url}; pass --version. \
     Last attempt: https://api.github.com/repos/{path}/tags?per_page=1: no release or tag in the answer";

fn no_service(_: &str) -> bool {
    false
}

fn has_service(_: &str) -> bool {
    true
}

fn no_repology(_: &str) -> Result<String, String> {
    Err("Repology has no newest version for it".to_string())
}

#[test]
pub(super) fn a_suse_repository_with_no_release_or_tag_is_no_upstream() {
    let spec = |_: &str| Ok(OPENSUSE_REPOS_SPEC.to_string());
    let repo = |url: &str| {
        Err(NO_RELEASE_ANSWER
            .replace("{url}", url)
            .replace("{path}", "openSUSE/openSUSE-repos"))
    };
    let lookups = Lookups {
        fetch_repo_latest: &repo,
        fetch_has_service: &has_service,
        ..unused_lookups()
    };
    let c = classify("openSUSE-repos", &spec, &lookups);
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::SuseNoUpstream {
            why: "https://github.com/openSUSE/openSUSE-repos has no release or tag".into(),
        })
    );
}

#[test]
pub(super) fn a_tagged_suse_repository_is_updated_like_any_other() {
    let spec = |_: &str| Ok(OPENSUSE_REPOS_SPEC.replace("20260423.1a6a0f3", "1.0"));
    let repo = |_: &str| Ok("2.0".to_string());
    let lookups = Lookups {
        fetch_repo_latest: &repo,
        ..unused_lookups()
    };
    let c = classify("openSUSE-repos", &spec, &lookups);
    assert!(
        matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "2.0"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_repository_outside_suse_with_no_release_stays_a_lookup_failure() {
    let spec_text = "\
Name:           transactional-wrapper
Version:        1.0
URL:            https://github.com/jsrain/transactional-wrapper
Source:         https://github.com/jsrain/transactional-wrapper/archive/v%{version}.tar.gz
";
    let spec = |_: &str| Ok(spec_text.to_string());
    let repo = |url: &str| {
        Err(NO_RELEASE_ANSWER
            .replace("{url}", url)
            .replace("{path}", "jsrain/transactional-wrapper"))
    };
    let lookups = Lookups {
        fetch_repo_latest: &repo,
        fetch_repology_latest: &no_repology,
        ..unused_lookups()
    };
    let c = classify("transactional-wrapper", &spec, &lookups);
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::LookupFailed { ref ecosystem_tag, .. })
            if ecosystem_tag == "git"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_source_with_no_url_and_no_service_is_no_upstream() {
    let spec = |_: &str| Ok(BRANDING_UPSTREAM_SPEC.to_string());
    let lookups = Lookups {
        fetch_repology_latest: &no_repology,
        fetch_has_service: &no_service,
        ..unused_lookups()
    };
    let c = classify("branding-upstream", &spec, &lookups);
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::SuseNoUpstream {
            why: "the Source: has no URL and no _service fetches it".into(),
        })
    );
}

#[test]
pub(super) fn a_source_with_no_url_but_a_service_stays_a_lookup_failure() {
    let spec = |_: &str| Ok(BRANDING_UPSTREAM_SPEC.to_string());
    let lookups = Lookups {
        fetch_repology_latest: &no_repology,
        fetch_has_service: &has_service,
        ..unused_lookups()
    };
    let c = classify("branding-upstream", &spec, &lookups);
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::LookupFailed { ref ecosystem_tag, .. })
            if ecosystem_tag == "Repology"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_link_package_with_a_computed_version_is_no_upstream() {
    let spec = |_: &str| Ok(FILESYSTEM_SPEC.to_string());
    let lookups = Lookups {
        fetch_repology_latest: &no_repology,
        fetch_has_service: &no_service,
        ..unused_lookups()
    };
    let c = classify("filesystem", &spec, &lookups);
    assert!(
        matches!(
            c.decision,
            Decision::Skipped(SkipReason::SuseNoUpstream { .. })
        ),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_source_with_a_url_is_not_no_upstream() {
    let spec = |_: &str| Ok(CACERT_SPEC.to_string());
    let lookups = Lookups {
        fetch_repology_latest: &no_repology,
        fetch_has_service: &no_service,
        ..unused_lookups()
    };
    let c = classify("ca-certificates-cacert", &spec, &lookups);
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::LookupFailed { ref ecosystem_tag, .. })
            if ecosystem_tag == "Repology"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn the_no_upstream_reason_is_shown_with_its_class() {
    let reason = SkipReason::SuseNoUpstream {
        why: "the Source: has no URL and no _service fetches it".into(),
    };
    assert_eq!(
        reason.to_string(),
        "SUSE-maintained, no upstream release: the Source: has no URL and no _service fetches it"
    );
}
