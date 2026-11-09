//! The plan report's counts and lines, as the sweep reports them.

use super::PlanReport;
use ap_build::update::{Candidate, Decision, SkipReason};
use ap_core::sink::NullReporter;

fn skipped(name: &str, reason: SkipReason) -> Candidate {
    Candidate {
        rpm_name: name.to_string(),
        decision: Decision::Skipped(reason),
        notes: Vec::new(),
    }
}

#[test]
fn a_failed_read_is_unreadable_and_a_404_is_not_on_obs() {
    let mut plan = PlanReport::default();
    plan.add(
        &skipped("a", SkipReason::ReadFailed("listing failed".into())),
        &NullReporter,
    );
    plan.add(
        &skipped("b", SkipReason::NotOnObs("gone".into())),
        &NullReporter,
    );
    plan.add(&skipped("c", SkipReason::NoSpec), &NullReporter);
    assert_eq!(plan.unreadable, ["a: listing failed"]);
    assert_eq!(plan.n_not_on_obs, 1);
    assert_eq!(plan.total(), 3);
    // NoSpec is a package found on purpose, so the run is not all-absent.
    assert!(!plan.only_absent());
}

#[test]
fn a_sweep_that_found_only_absent_or_no_packages_is_not_a_checked_one() {
    assert!(PlanReport::default().only_absent());
    let absent = PlanReport {
        n_skipped: 2,
        n_not_on_obs: 2,
        ..Default::default()
    };
    assert!(absent.only_absent());
    let mixed = PlanReport {
        n_skipped: 2,
        n_not_on_obs: 1,
        ..Default::default()
    };
    assert!(!mixed.only_absent());
    let skipped_on_purpose = PlanReport {
        n_skipped: 1,
        ..Default::default()
    };
    assert!(!skipped_on_purpose.only_absent());
    let found = PlanReport {
        n_update: 1,
        ..Default::default()
    };
    assert!(!found.only_absent());
}

#[test]
fn the_progress_reports_carry_the_total_once_known() {
    use ap_core::sink::Reporter;
    use std::sync::Mutex;
    struct Record(Mutex<Vec<ap_core::event::Event>>);
    impl Reporter for Record {
        fn report(&self, event: &ap_core::event::Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }
    let rec = Record(Mutex::new(Vec::new()));
    let mut plan = PlanReport {
        total: Some(3),
        ..Default::default()
    };
    plan.add(&skipped("a", SkipReason::NoSpec), &rec);
    plan.finish(&rec);
    let events = rec.0.lock().unwrap();
    let totals: Vec<Option<usize>> = events
        .iter()
        .filter_map(|e| match e {
            ap_core::event::Event::SweepProgress { total, .. } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(totals, [Some(3), Some(3)]);
}

#[test]
fn the_plan_line_breaks_the_skips_down_by_reason() {
    let mut plan = PlanReport::default();
    plan.add(&skipped("a", SkipReason::NoSpec), &NullReporter);
    plan.add(&skipped("b", SkipReason::NoSpec), &NullReporter);
    plan.add(
        &skipped("c", SkipReason::ReadFailed("interrupted".into())),
        &NullReporter,
    );
    assert_eq!(plan.by_reason.get("NoSpec"), Some(&2));
    assert_eq!(plan.by_reason.get("stopped"), Some(&1));
    assert_eq!(super::skip_label(&SkipReason::NoSpec), "NoSpec");
    let lookup = SkipReason::LookupFailed {
        ecosystem_tag: "PyPI".into(),
        name: "x".into(),
        error: "404".into(),
    };
    assert_eq!(super::skip_label(&lookup), "LookupFailed");
    assert_eq!(SkipReason::NotOnObs("gone".into()).kind(), "NotOnObs");
}

#[test]
fn a_lookup_an_outage_failed_is_incomplete_not_a_skip() {
    let mut plan = PlanReport::default();
    plan.add(
        &skipped(
            "mailchariot",
            SkipReason::LookupFailed {
                ecosystem_tag: "GitRepo".into(),
                name: "https://example.org/x".into(),
                error: "http status: 503".into(),
            },
        ),
        &NullReporter,
    );
    plan.add(
        &skipped(
            "gone",
            SkipReason::LookupFailed {
                ecosystem_tag: "PyPI".into(),
                name: "gone".into(),
                error: "http status: 404".into(),
            },
        ),
        &NullReporter,
    );
    assert_eq!(plan.unreadable.len(), 1);
    assert!(plan.unreadable[0].starts_with("mailchariot: "));
    assert_eq!(plan.n_skipped, 2);
}
