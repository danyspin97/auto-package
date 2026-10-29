
use super::*;

fn fin(pkg: &str, outcome: PackageOutcome) -> Event {
    Event::PackageFinished {
        pkg: pkg.to_string(),
        version: "1.0".to_string(),
        outcome,
    }
}

fn failed(label: &str, after: usize) -> AddFailure {
    AddFailure {
        label: label.to_string(),
        message: format!("{label}: boom"),
        after,
    }
}

// A dependency row is started without a request name: nothing is noted
// for it unless the dependency queue notes one (see `spawn_dep_build`).
fn no_origin(_row: &str) -> Option<String> {
    None
}

fn labels(failures: &[AddFailure]) -> Vec<&str> {
    failures.iter().map(|f| f.label.as_str()).collect()
}

#[test]
fn finished_since_only_looks_past_the_mark() {
    let events = vec![
        fin("python-a", PackageOutcome::Built),
        fin(
            "python-b",
            PackageOutcome::Failed {
                reason: "x".into(),
                disk_full: false,
            },
        ),
    ];
    assert!(finished_since(&events, 0, "python-a"));
    assert!(!finished_since(&events, 1, "python-a"));
    assert!(finished_since(&events, 1, "python-b"));
    assert!(!finished_since(&events, 2, "python-b"));
    assert!(!finished_since(&events, 0, "python-c"));
}

#[test]
fn failure_is_withdrawn_when_its_package_is_later_built_as_a_dependency() {
    // The request `foo` fails, then `python-foo` builds as a dependency.
    let events = vec![
        fin(
            "python-foo",
            PackageOutcome::Failed {
                reason: "x".into(),
                disk_full: false,
            },
        ),
        fin("python-foo", PackageOutcome::Built),
    ];
    let mut failures = vec![failed("foo", 1)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert!(failures.is_empty());
}

#[test]
fn a_dependency_build_settles_the_request_under_the_pypi_name_only() {
    // `foo` fails; `python-bar` builds as a dependency: `foo` stands.
    // `python-foo` builds as a dependency: `foo` is withdrawn.
    let events = vec![fin("python-bar", PackageOutcome::Built)];
    let mut failures = vec![failed("foo", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert_eq!(labels(&failures), vec!["foo"]);
    let events = vec![fin("python-foo", PackageOutcome::Built)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert!(failures.is_empty());
}

#[test]
fn a_recorded_origin_names_the_row_over_the_rpm_name_guess() {
    // The row was requested as `baz`; a failure of `foo` is not its.
    let origin = |row: &str| (row == "python-foo").then(|| "baz".to_string());
    let events = vec![fin("python-foo", PackageOutcome::Built)];
    let mut failures = vec![failed("foo", 0), failed("baz", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), origin);
    assert_eq!(labels(&failures), vec!["foo"]);
}

#[test]
fn a_dependency_label_settles_a_request_only_under_its_own_name() {
    // `python-foo` was built as the dependency `foo`: it settles a failed
    // request `foo`. `python-bar` settles nothing of `foo`.
    let label = |row: &str| (row == "python-foo").then(|| "foo".to_string());
    let events = vec![fin("python-bar", PackageOutcome::Built)];
    let mut failures = vec![failed("foo", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), label);
    assert_eq!(labels(&failures), vec!["foo"]);
    let events = vec![fin("python-foo", PackageOutcome::Built)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), label);
    assert!(failures.is_empty());
}

#[test]
fn a_retry_resends_only_the_name_its_row_was_requested_with() {
    let origin = |row: &str| (row == "python-foo").then(|| "foo".to_string());
    // A new request names itself.
    let (raw, retry_of) = request_names(tui::AddRequest::New("bar".into()), no_origin);
    assert_eq!((raw.as_deref(), retry_of), (Some("bar"), None));
    // A retry of a root row resends its request name.
    let (raw, retry_of) = request_names(tui::AddRequest::Retry("python-foo".into()), origin);
    assert_eq!(
        (raw.as_deref(), retry_of.as_deref()),
        (Some("foo"), Some("python-foo"))
    );
    // A retry of a dependency row has no name to resend: it builds nothing.
    let (raw, retry_of) = request_names(tui::AddRequest::Retry("python-trantor".into()), no_origin);
    assert_eq!(raw, None);
    assert_eq!(retry_of.as_deref(), Some("python-trantor"));
}

#[test]
fn failure_is_withdrawn_by_the_rpm_name_too() {
    let events = vec![fin("python-foo", PackageOutcome::AlreadyBuilt)];
    let mut failures = vec![failed("python-foo", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert!(failures.is_empty());
}

#[test]
fn failure_after_the_build_stands() {
    // Built first, then the request failed: the later failure is not settled.
    let events = vec![fin("python-foo", PackageOutcome::Built)];
    let mut failures = vec![failed("foo", 1)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert_eq!(labels(&failures), vec!["foo"]);
}

#[test]
fn failure_stands_when_the_package_did_not_end_built() {
    let events = vec![
        fin("python-foo", PackageOutcome::Built),
        fin(
            "python-foo",
            PackageOutcome::Failed {
                reason: "x".into(),
                disk_full: false,
            },
        ),
    ];
    let mut failures = vec![failed("foo", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert_eq!(labels(&failures), vec!["foo"]);
}

#[test]
fn failure_of_another_package_stands() {
    // `python-bar` is the RPM name of `bar` only, so neither `foo` nor
    // `baz` is settled by it.
    let events = vec![fin("python-bar", PackageOutcome::Built)];
    let mut failures = vec![failed("foo", 0), failed("baz", 0)];
    withdraw_settled_failures(&mut failures, &row_ends(&events), no_origin);
    assert_eq!(labels(&failures), vec!["foo", "baz"]);
}
