//! The sibling index of a project of 200 packages: each spec is read from OBS
//! once for the plan, and the plan is the same for every pool size.

use super::super::check_pool::check_in_order;
use super::super::plan::skipped;
use super::super::planned_specs::{SpecCells, read_once};
use super::*;
use crate::update::helpers::only_repo;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Package `i` of the project, and its spec. Packages `i` and `j` share a
/// forge repository when they are congruent mod 5; one package in seven is on
/// the newer major 2.1.0, the others on 1.4.0.
fn package(i: usize) -> (String, String) {
    let name = format!("pkg-{i:03}");
    let version = if i % 7 == 0 { "2.1.0" } else { "1.4.0" };
    let text = format!(
        "Name:           {name}\nVersion:        {version}\n\
         Source:         https://github.com/org/repo{}/archive/v{version}.tar.gz\n",
        i % 5
    );
    (name, text)
}

/// The plan of the project for a pool of `jobs`, as each answer is handed on,
/// and how many times OBS was asked for each spec. `flaky` is a package whose
/// read always fails transiently.
fn plan_with(jobs: usize, flaky: &str) -> (Vec<String>, BTreeMap<String, usize>) {
    // Two packages have no spec at all (`pkg-013`, `pkg-113`): OBS is asked
    // about them once too.
    let all: Vec<(String, String)> = (0..200).map(package).collect();
    let names: Vec<String> = all.iter().map(|(n, _)| n.clone()).collect();
    let files: BTreeMap<String, String> = all
        .into_iter()
        .filter(|(n, _)| !n.ends_with("13"))
        .collect();
    let asked = Mutex::new(BTreeMap::new());
    let obs = |name: &str| {
        *asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.to_string())
            .or_insert(0usize) += 1;
        if name == flaky {
            return Err(ReadError::Failed("timed out".into()));
        }
        files.get(name).cloned().ok_or(ReadError::NoSpec)
    };
    let cells: SpecCells = Mutex::new(HashMap::new());
    let fetch = |name: &str| read_once(&cells, name, || obs(name));
    let siblings = ProjectSiblings::new(names.clone(), jobs, &fetch);
    let check = |name: &str| classify_in_project(name, &fetch, &only_repo("2.2.0"), &siblings);
    let panicked = |name: &str, why: &str| {
        skipped(
            name,
            SkipReason::LookupFailed {
                ecosystem_tag: "check".into(),
                name: name.into(),
                error: format!("the check panicked: {why}"),
            },
        )
    };
    let mut plan = Vec::new();
    check_in_order(&names, jobs, &check, &panicked, &|_| {}, &mut |_, c| {
        plan.push(format!("{} {:?}", c.rpm_name, c.decision));
        true
    });
    let asked = asked.into_inner().unwrap_or_else(|e| e.into_inner());
    (plan, asked)
}

#[test]
fn a_project_is_read_once_and_planned_the_same_for_any_pool_size() {
    let (one, asked_one) = plan_with(1, "");
    let (eight, asked_eight) = plan_with(8, "");
    assert_eq!(one, eight);
    assert_eq!(asked_one.len(), 200);
    assert!(asked_one.values().all(|&n| n == 1), "{asked_one:?}");
    assert_eq!(asked_eight, asked_one);
    // The sibling check ran: a package on 1.4.0 next to one on 2.1.0 is held.
    assert!(one.iter().any(|l| l.contains("OtherSeries")), "{one:?}");
}

#[test]
fn a_package_without_a_forge_upstream_reads_no_other_spec() {
    let spec = "Name:           python-x\nVersion:        1.0.0\n\
        Source:         https://files.pythonhosted.org/packages/source/x/x/x-1.0.0.tar.gz\n";
    let asked = Mutex::new(Vec::new());
    let fetch = |n: &str| {
        asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(n.to_string());
        if n == "python-x" {
            Ok(spec.to_string())
        } else {
            Err(ReadError::NoSpec)
        }
    };
    let names: Vec<String> = (0..200).map(|i| package(i).0).collect();
    let siblings = ProjectSiblings::new(names.clone(), 8, &fetch);
    let lookups = crate::update::helpers::only_pypi("2.0.0");
    let c = classify_in_project("python-x", &fetch, &lookups, &siblings);
    assert!(
        matches!(c.decision, Decision::Update { .. }),
        "{:?}",
        c.decision
    );
    assert_eq!(
        asked.into_inner().unwrap_or_else(|e| e.into_inner()),
        ["python-x"]
    );
}

#[test]
fn a_failed_read_of_a_package_stops_the_checks_it_could_hold_for_any_pool_size() {
    let (one, _) = plan_with(1, "pkg-042");
    let (eight, _) = plan_with(8, "pkg-042");
    assert_eq!(one, eight);
    assert!(one.iter().any(|l| l.contains("ReadFailed")), "{one:?}");
}

/// A spec of `name` at `version`, whose source is the GitHub repository `repo`.
fn forged(name: &str, version: &str, repo: &str) -> String {
    format!(
        "Name:           {name}\nVersion:        {version}\n\
         Source:         https://github.com/{repo}/archive/v{version}.tar.gz\n"
    )
}

/// The decision for `influxdb` 1.8.10, whose upstream is newer at 2.7.12,
/// beside `influxdb2`, whose spec is `sibling(n)` for its `n`-th read.
fn influx_decision(sibling: &(dyn Fn(usize) -> Result<String, ReadError> + Sync)) -> Decision {
    let own = forged("influxdb", "1.8.10", "influxdata/influxdb");
    let calls = AtomicUsize::new(0);
    let fetch = |name: &str| match name {
        "influxdb" => Ok(own.clone()),
        "influxdb2" => sibling(calls.fetch_add(1, Ordering::SeqCst)),
        _ => Err(ReadError::NoSpec),
    };
    let names = vec!["influxdb".to_string(), "influxdb2".to_string()];
    let siblings = ProjectSiblings::new(names, 1, &fetch);
    classify_in_project("influxdb", &fetch, &only_repo("2.7.12"), &siblings).decision
}

#[test]
fn a_healthy_sibling_holds_the_candidate_back() {
    let healthy = forged("influxdb2", "2.7.10", "influxdata/influxdb");
    let d = influx_decision(&|_| Ok(healthy.clone()));
    assert!(
        matches!(d, Decision::Skipped(SkipReason::NeedsMaintainer(_))),
        "{d:?}"
    );
}

#[test]
fn a_sibling_whose_read_failed_stops_the_candidate_it_could_hold() {
    // Without the read, the candidate would be released as a plain update.
    let d = influx_decision(&|_| Err(ReadError::Failed("timed out".into())));
    assert!(
        matches!(d, Decision::Skipped(SkipReason::ReadFailed(_))),
        "{d:?}"
    );
}

#[test]
fn an_interrupted_sibling_read_stops_the_candidates_that_need_it() {
    let d = influx_decision(&|_| Err(ReadError::Failed("interrupted".into())));
    assert!(
        matches!(d, Decision::Skipped(SkipReason::ReadFailed(ref why)) if why.contains("(interrupted)")),
        "{d:?}"
    );
}

#[test]
fn a_sibling_read_that_fails_once_is_read_again_before_the_index_is_used() {
    let healthy = forged("influxdb2", "2.7.10", "influxdata/influxdb");
    let d = influx_decision(&|n| {
        if n == 0 {
            Err(ReadError::Failed("timed out".into()))
        } else {
            Ok(healthy.clone())
        }
    });
    assert!(
        matches!(d, Decision::Skipped(SkipReason::NeedsMaintainer(_))),
        "{d:?}"
    );
}

#[test]
fn a_panicking_sibling_read_is_an_unreadable_package_not_a_panic() {
    let d = influx_decision(&|_| -> Result<String, ReadError> { panic!("boom") });
    assert!(
        matches!(d, Decision::Skipped(SkipReason::ReadFailed(_))),
        "{d:?}"
    );
}

#[test]
fn an_abandoned_index_reads_nothing_and_stops_the_checks_that_need_it() {
    let reads = AtomicUsize::new(0);
    let fetch = |_: &str| {
        reads.fetch_add(1, Ordering::SeqCst);
        Err(ReadError::NoSpec)
    };
    let names = vec!["a".to_string(), "b".to_string()];
    let siblings = ProjectSiblings::new(names, 2, &fetch);
    siblings.abandon();
    assert!(siblings.related("a", "https://github.com/x/y").is_err());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}
