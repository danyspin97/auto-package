use super::*;

use super::helpers::*;

/// AppCo versioned-library packages are named `<base>+<version>` on OBS
/// but hold a plain `<base>.spec`. The pre-Oct-6 picker guessed
/// `<pkg>.spec` and silently missed the real one on every such package.
#[test]
pub(super) fn picks_the_bare_name_spec_for_an_appco_versioned_package() {
    let files = vec![
        "_service".to_string(),
        "pytorch.spec".to_string(),
        "pytorch.changes".to_string(),
    ];
    assert_eq!(
        pick_spec_filename("pytorch+2.4", &files).as_deref(),
        Some("pytorch.spec"),
    );
}

/// For the common `python-foo` case the file matching `<pkg>.spec` must
/// still win over anything alphabetically earlier (e.g. a `_service.spec`
/// placeholder or a related subpackage's spec).
#[test]
pub(super) fn picks_the_pkg_name_spec_when_present_for_the_ordinary_case() {
    let files = vec![
        "_other.spec".to_string(),
        "python-attrs.spec".to_string(),
        "python-attrs.changes".to_string(),
    ];
    assert_eq!(
        pick_spec_filename("python-attrs", &files).as_deref(),
        Some("python-attrs.spec"),
    );
}

#[test]
pub(super) fn picks_alphabetically_first_spec_when_neither_canonical_name_is_present() {
    let files = vec![
        "zzz.spec".to_string(),
        "aaa.spec".to_string(),
        "mmm.spec".to_string(),
    ];
    assert_eq!(
        pick_spec_filename("something-weird+1", &files).as_deref(),
        Some("aaa.spec"),
    );
}

#[test]
pub(super) fn no_spec_file_at_all_returns_none() {
    let files = vec!["_service".to_string(), "README.md".to_string()];
    assert_eq!(pick_spec_filename("whatever", &files), None);
}

/// A version that is neither obviously newer nor obviously the same —
/// because one side cannot be parsed as PEP 440 at all — must NOT be
/// labelled `UpToDate`. The Oct 6 review reproduced the false-current
/// label with a spec whose `Version: %{package_version}` never
/// expanded; the comparison was between the literal macro and a real
/// PyPI version, so parsing failed, so the pre-Oct-6 code concluded
/// "not newer, therefore current".
#[test]
pub(super) fn an_unparseable_version_is_not_reported_as_up_to_date() {
    let spec = "\
Name:           python-weird
Version:        not-a-valid-pep-440
Source:         https://files.pythonhosted.org/packages/source/w/weird/weird-%{version}.tar.gz
";
    let spec_fn = |_: &str| Ok(spec.to_string());
    let c = classify("python-weird", &spec_fn, &only_pypi("2.0.0"));
    match c.decision {
        Decision::Skipped(SkipReason::VersionCompareFailed { packaged, latest }) => {
            assert_eq!(packaged, "not-a-valid-pep-440");
            assert_eq!(latest, "2.0.0");
        }
        other => panic!(
            "expected Skipped(VersionCompareFailed), got {other:?} — this is the \
                 Oct 6 false-UpToDate regression"
        ),
    }
}

/// A spec using an AppCo `Version: %{package_version}` pattern should
/// resolve against the matching `%define`/`%global` earlier in the
/// file, so the comparison sees the real version. Without this the
/// version string reaching the comparator is literally
/// `%{package_version}`, which triggers the VersionCompareFailed path
/// above (which is at least not-silent, but still blocks genuine
/// updates).
#[test]
pub(super) fn version_macro_is_expanded_from_a_preceding_define() {
    let spec = "\
%define package_version 2.4.0
Name:           pytorch
Version:        %{package_version}
Source:         https://files.pythonhosted.org/packages/source/p/pytorch/pytorch-%{version}.tar.gz
";
    let spec_fn = |_: &str| Ok(spec.to_string());
    let c = classify("pytorch+2.4", &spec_fn, &only_pypi("2.5.0"));
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "pytorch".into()
            },
            old_version: "2.4.0".into(),
            new_version: "2.5.0".into(),
            upstream_tag: None,
        },
        "expected the define-resolved 2.4.0 to compare against 2.5.0"
    );
}
