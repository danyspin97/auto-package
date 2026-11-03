use super::*;

use crate::update::helpers::{ATTRS_SPEC, unused_lookups};
use crate::update::{Candidate, classify};
use std::cell::Cell;

fn answers(newest: &'static str) -> impl Fn(&str) -> Result<String, String> {
    move |_: &str| Ok(newest.to_string())
}

fn no_answer(_: &str) -> Result<String, String> {
    Err("Repology has no newest version for it".to_string())
}

fn check(target: &str, repology: &'static str) -> Verdict {
    verdict("attrs", target, Scheme::Generic, &answers(repology))
}

#[test]
fn equal_versions_agree() {
    assert_eq!(check("1.2.3", "1.2.3"), Verdict::Agree);
}

#[test]
fn an_older_repology_version_is_ignored() {
    assert_eq!(check("2.0", "1.9"), Verdict::Agree);
}

#[test]
fn a_patch_level_difference_is_a_note_not_a_stop() {
    assert_eq!(
        check("1.2.3", "1.2.4"),
        Verdict::PatchLevel {
            repology: "1.2.4".into()
        }
    );
}

#[test]
fn a_newer_minor_on_repology_disagrees() {
    assert_eq!(
        check("1.2.0", "1.3.0"),
        Verdict::Disagrees {
            repology: "1.3.0".into()
        }
    );
}

#[test]
fn a_newer_major_on_repology_disagrees() {
    assert_eq!(
        check("1.9", "2.0"),
        Verdict::Disagrees {
            repology: "2.0".into()
        }
    );
}

#[test]
fn a_repology_error_has_no_effect() {
    assert_eq!(
        verdict("attrs", "1.2.3", Scheme::Generic, &no_answer),
        Verdict::Agree
    );
}

#[test]
fn the_lookup_is_asked_for_the_project_name() {
    let asked = Cell::new(false);
    let fetch = |name: &str| -> Result<String, String> {
        asked.set(name == "attrs");
        Err("unknown project".to_string())
    };
    assert_eq!(
        verdict("attrs", "1.2.3", Scheme::Generic, &fetch),
        Verdict::Agree
    );
    assert!(asked.get());
}

/// Lookups for a PyPI package: PyPI says `pypi`, Repology says `repology`.
fn pypi_and_repology(pypi: &'static str, repology: &'static str) -> Lookups<'static> {
    let p: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_: &str| Ok(pypi.to_string())));
    let r: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_: &str| Ok(repology.to_string())));
    Lookups {
        fetch_pypi_latest: p,
        fetch_repology_latest: r,
        ..unused_lookups()
    }
}

fn classified(lookups: &Lookups<'_>) -> Candidate {
    let spec = |_: &str| Ok(ATTRS_SPEC.to_string());
    classify("python-attrs", &spec, lookups)
}

#[test]
fn a_repology_minor_disagreement_stops_the_update() {
    let c = classified(&pypi_and_repology("27.0.0", "27.1.0"));
    assert_eq!(
        c.decision,
        Decision::Skipped(SkipReason::RepologyDisagrees {
            source: "pypi".into(),
            target: "27.0.0".into(),
            repology: "27.1.0".into(),
        })
    );
    assert!(c.notes.is_empty());
}

#[test]
fn a_repology_patch_difference_keeps_the_update_and_notes_it() {
    let c = classified(&pypi_and_repology("27.0.0", "27.0.1"));
    assert!(
        matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "27.0.0"),
        "{c:?}"
    );
    assert_eq!(c.notes.len(), 1, "{c:?}");
    assert!(c.notes[0].contains("27.0.1"), "{c:?}");
}

#[test]
fn a_calendar_version_on_either_side_agrees() {
    assert_eq!(check("2024.05", "2024.10"), Verdict::Agree);
    assert_eq!(check("2024.10.1", "2025.1"), Verdict::Agree);
    assert_eq!(check("20240101", "20240201"), Verdict::Agree);
    assert_eq!(check("1.2.0", "2024.05"), Verdict::Agree);
    assert_eq!(check("1.2.0", "20240101"), Verdict::Agree);
}

#[test]
fn a_calendar_version_is_not_looked_up() {
    let asked = Cell::new(false);
    let fetch = |_: &str| -> Result<String, String> {
        asked.set(true);
        Ok("2025.1".to_string())
    };
    assert_eq!(
        verdict("attrs", "2024.05", Scheme::Generic, &fetch),
        Verdict::Agree
    );
    assert!(!asked.get());
}

#[test]
fn only_a_four_or_eight_digit_year_first_is_a_date() {
    assert!(is_date_like("2024.05"));
    assert!(is_date_like("2024.10.1"));
    assert!(is_date_like("20240101"));
    assert!(is_date_like("1999"));
    assert!(!is_date_like("1.2.3"));
    assert!(!is_date_like("20.5"));
    assert!(!is_date_like("2100.1"));
    assert!(!is_date_like("202405"));
    assert!(!is_date_like("v2024.1"));
}

#[test]
fn a_normal_minor_disagreement_still_disagrees() {
    assert_eq!(
        check("1.2.0", "1.3.0"),
        Verdict::Disagrees {
            repology: "1.3.0".into()
        }
    );
}
