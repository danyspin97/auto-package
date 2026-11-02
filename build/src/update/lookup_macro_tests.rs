//! Lookup names and URLs built from a spec's own macros: the three real specs
//! that Repology or the forge route got wrong, and the refusal of a text that
//! still holds a macro.

use super::*;

use super::helpers::*;

const LIBDBI_SPEC: &str = include_str!("../testdata/spec/libdbi.spec");
const ARANGODB3_SPEC: &str = include_str!("../testdata/spec/arangodb3.spec");
const GALERA_CLUSTERCHECK_SPEC: &str = include_str!("../testdata/spec/galera-clustercheck.spec");

#[test]
pub(super) fn an_undefined_conditional_macro_in_the_name_is_empty() {
    // `Name: libdbi%{?psuffix}`: psuffix is only defined under a %if that
    // cannot be evaluated, so the default flavour has no suffix.
    let header = spec::parse_header(LIBDBI_SPEC).unwrap();
    assert_eq!(header.name, "libdbi");
    let spec = |_: &str| Ok(LIBDBI_SPEC.to_string());
    let repology: &'static dyn Fn(&str) -> Result<String, String> = &|name: &str| {
        assert_eq!(name, "libdbi");
        Ok("0.9.1".to_string())
    };
    let c = classify(
        "libdbi",
        &spec,
        &lookups_for(&|_| Err("no".into()), repology),
    );
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::Repology {
                project: "libdbi".into()
            },
            old_version: "0.9.0.g33".into(),
            new_version: "0.9.1".into(),
            upstream_tag: None,
        }
    );
}

#[test]
pub(super) fn a_forge_url_with_a_define_is_looked_up_expanded() {
    // `Url: https://github.com/%{srcname}/%{srcname}` and a Source0 built from
    // the same define: the forge lookup gets the expanded repository URL.
    let header = spec::parse_header(ARANGODB3_SPEC).unwrap();
    assert_eq!(
        header.url.as_deref(),
        Some("https://github.com/arangodb/arangodb")
    );
    assert!(
        ecosystems(&header, None).contains(&Ecosystem::GitRepo {
            url: "https://github.com/arangodb/arangodb".into()
        }),
        "{:?}",
        ecosystems(&header, None)
    );
    let spec = |_: &str| Ok(ARANGODB3_SPEC.to_string());
    let repo: &'static dyn Fn(&str) -> Result<String, String> = &|url: &str| {
        assert_eq!(url, "https://github.com/arangodb/arangodb");
        Ok("3.10.5".to_string())
    };
    let lookups = Lookups {
        fetch_repo_latest: repo,
        ..unused_lookups()
    };
    let c = classify("arangodb3", &spec, &lookups);
    assert_eq!(
        c.decision,
        Decision::Update {
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/arangodb/arangodb".into()
            },
            old_version: "3.10.4".into(),
            new_version: "3.10.5".into(),
            upstream_tag: None,
        }
    );
}

#[test]
pub(super) fn a_malformed_url_in_the_spec_is_passed_on_as_written() {
    // galera-clustercheck's `Url:` is malformed in the spec itself (a `.` where
    // a `/` belongs). No macro is involved, so nothing is refused: the text is
    // looked up as written. Its `+git0.8cafd6f` version is a snapshot, so the
    // newest release says nothing about it.
    let header = spec::parse_header(GALERA_CLUSTERCHECK_SPEC).unwrap();
    assert_eq!(
        header.url.as_deref(),
        Some("https://github.com/darix/galera-clustercheck.0.0+git1444039416.")
    );
    let spec = |_: &str| Ok(GALERA_CLUSTERCHECK_SPEC.to_string());
    let c = classify("galera-clustercheck", &spec, &only_repo("0.0.3"));
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::Snapshot { .. })),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_lookup_name_that_keeps_a_macro_is_refused_and_named() {
    let spec_text = "Name: foo%{!?bar}\nVersion: 1.0\n";
    let spec = |_: &str| Ok(spec_text.to_string());
    let repology: &'static dyn Fn(&str) -> Result<String, String> =
        &|_: &str| Err("the lookup should not be made".to_string());
    let c = classify("foo", &spec, &lookups_for(&|_| Err("no".into()), repology));
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::UnexpandedMacro {
            lookup: "Repology".into(),
            text: "foo%{!?bar}".into(),
        })
    );
    let Decision::Skipped(reason) = &c.decision else {
        panic!("{:?}", c.decision)
    };
    assert!(reason.to_string().contains("\"foo%{!?bar}\""), "{reason}");
}

#[test]
pub(super) fn conditional_macros_expand_against_the_spec_defines() {
    let spec_text = "\
Name:           pkg%{?sfx}
%define set 1
%define host example.org
Version:        1.0
Url:            https://%{host}/%{?missing}x%{?set:/defined}%{!?set:/undefined}%{!?missing:/absent}
";
    let header = spec::parse_header(spec_text).unwrap();
    assert_eq!(header.name, "pkg");
    assert_eq!(
        header.url.as_deref(),
        Some("https://example.org/x/defined/absent")
    );
}

#[test]
pub(super) fn a_macro_left_after_expansion_is_detected_but_a_percent_escape_is_not() {
    assert!(spec::has_unexpanded_macro("foo%{!?bar}"));
    assert!(spec::has_unexpanded_macro("x%(date)"));
    assert!(spec::has_unexpanded_macro("x%%y"));
    assert!(spec::has_unexpanded_macro("x%name"));
    assert!(!spec::has_unexpanded_macro(
        "https://example.org/a%20b%2Bc.tar.gz"
    ));
}
