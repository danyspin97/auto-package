use std::cell::Cell;

use super::gather::{ActionInputs, gather};
use super::upstream_check::{upstream_report, version_findings};
use super::{Severity, ids};
use crate::update::{Candidate, Decision, Ecosystem, SkipReason};

fn cand(decision: Decision) -> Candidate {
    Candidate {
        rpm_name: "cracklib".into(),
        decision,
        notes: Vec::new(),
    }
}

fn pypi() -> Ecosystem {
    Ecosystem::Pypi {
        pypi_name: "x".into(),
    }
}

fn update(latest: &str) -> Decision {
    Decision::Update {
        ecosystem: pypi(),
        old_version: "2.10.4".into(),
        new_version: latest.into(),
        upstream_tag: None,
    }
}

fn up_to_date() -> Decision {
    Decision::UpToDate {
        ecosystem: pypi(),
        version: "2.10.4".into(),
    }
}

#[test]
fn a_newer_upstream_release_is_behind_upstream() {
    let (f, _) = version_findings("cracklib", "2.10.4", &cand(update("2.11.0")), None);
    assert_eq!(f.len(), 1);
    assert_eq!(
        (f[0].id, f[0].severity),
        (ids::VERSION_BEHIND_UPSTREAM, Severity::Warning)
    );
    assert!(
        f[0].message
            .contains("upstream has 2.11.0; this request packages 2.10.4")
    );
}

#[test]
fn the_packaged_version_being_current_gives_nothing() {
    let (f, s) = version_findings("cracklib", "2.10.4", &cand(up_to_date()), None);
    assert!(f.is_empty() && s.is_empty());
}

#[test]
fn a_skipped_planner_answer_is_a_note_with_the_reason() {
    let reason = SkipReason::Prerelease {
        packaged: "2.10.4".into(),
        latest: "3.0rc1".into(),
    };
    let text = reason.to_string();
    let (f, _) = version_findings("cracklib", "2.10.4", &cand(Decision::Skipped(reason)), None);
    assert_eq!(
        (f[0].id, f[0].severity),
        (ids::VERSION_NOT_CHECKED, Severity::Note)
    );
    assert!(f[0].message.contains(&text));
}

#[test]
fn repology_newer_than_both_is_a_note_and_a_failure_is_only_said() {
    let ask = |r| version_findings("cracklib", "2.10.4", &cand(up_to_date()), Some(r));
    let (f, _) = ask(Ok("2.12.0".into()));
    assert_eq!(
        (f[0].id, f[0].severity),
        (ids::VERSION_REPOLOGY_NEWER, Severity::Note)
    );
    assert!(ask(Ok("2.10.4".into())).0.is_empty());
    // Not newer than what the primary lookup already says.
    let (f, _) = version_findings(
        "cracklib",
        "2.10.4",
        &cand(update("2.12.0")),
        Some(Ok("2.12.0".into())),
    );
    assert_eq!(f.len(), 1);
    let (f, s) = ask(Err("down".into()));
    assert!(f.is_empty());
    assert_eq!(s[0].check, "Repology");
}

fn inputs() -> ActionInputs {
    let o = super::gather_tests::obs();
    gather(&o, "1394662").unwrap().actions.remove(0)
}

#[test]
fn the_planner_failure_is_an_incomplete_warning() {
    let a = inputs();
    let (f, _) = upstream_report(
        &a,
        &|_| Err("no network".into()),
        &|_| Err("unused".into()),
        &|_| true,
    );
    assert_eq!(
        (f[0].id, f[0].severity),
        (ids::REVIEW_INCOMPLETE, Severity::Warning)
    );
}

#[test]
fn repology_is_asked_only_when_the_primary_was_not_repology() {
    let a = inputs();
    let asked = Cell::new(0);
    let repology = |_: &str| {
        asked.set(asked.get() + 1);
        Ok("9.9".to_string())
    };
    upstream_report(&a, &|_| Ok(cand(up_to_date())), &repology, &|_| true);
    let after_primary_pypi = asked.get();
    assert_eq!(after_primary_pypi, 1, "Repology is the second opinion");
    let by_repology = Decision::UpToDate {
        ecosystem: Ecosystem::Repology {
            project: "cracklib".into(),
        },
        version: "2.10.4".into(),
    };
    upstream_report(&a, &|_| Ok(cand(by_repology.clone())), &repology, &|_| true);
    // The second call asked nothing more.
    assert_eq!(asked.get(), after_primary_pypi);
}

#[test]
fn a_host_the_spec_names_that_is_not_allowed_is_not_contacted() {
    let mut a = inputs();
    let called = Cell::new(false);
    let plan = |_: &ActionInputs| {
        called.set(true);
        Ok(cand(up_to_date()))
    };
    let allowed = |h: &str| h == "github.com";
    // github.com is allowed: the planner runs.
    let (_, _) = upstream_report(&a, &plan, &|_| Err("off".into()), &allowed);
    assert!(called.get());
    called.set(false);
    // The Source and URL name another host.
    for text in a.new.texts.values_mut() {
        *text = text.replace("github.com", "gitlab.evil.example");
    }
    let (found, _) = upstream_report(&a, &plan, &|_| Err("off".into()), &allowed);
    assert!(!called.get());
    assert_eq!(
        (found[0].id, found[0].severity),
        (ids::VERSION_NOT_CHECKED, Severity::Note)
    );
    assert!(
        found[0].message.contains("gitlab.evil.example"),
        "{}",
        found[0].message
    );
    // A _service repository counts too.
    let mut b = inputs();
    b.new.texts.insert(
        "_service".into(),
        "<services><service name=\"obs_scm\"><param name=\"url\">https://10.0.0.1:8080/x.git</param></service></services>".into(),
    );
    let (found, _) = upstream_report(&b, &plan, &|_| Err("off".into()), &allowed);
    assert!(found[0].message.contains("10.0.0.1"));
    assert!(!called.get());
}
