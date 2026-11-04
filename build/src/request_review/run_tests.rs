use super::gather_tests::obs;
use super::run::{Deps, Options, review};
use super::{Severity, ids};
use crate::update::{Candidate, Decision, Ecosystem};

fn up_to_date(_: &super::gather::ActionInputs) -> Result<Candidate, String> {
    Ok(Candidate {
        rpm_name: "cracklib".into(),
        decision: Decision::UpToDate {
            ecosystem: Ecosystem::Pypi {
                pypi_name: "x".into(),
            },
            version: "2.10.4".into(),
        },
        notes: Vec::new(),
    })
}

fn deps() -> Deps<'static> {
    Deps {
        plan: &up_to_date,
        repology: &|_| Ok("2.10.4".to_string()),
        ldconfig_macro: &|_| true,
        host_allowed: &|_| true,
    }
}

fn options<'a>(scratch: &'a std::path::Path, download: bool) -> Options<'a> {
    Options {
        id: "1394662",
        download,
        scratch,
        fail_on: Severity::Warning,
        fail_on_agent: false,
        build: "test",
    }
}

#[test]
fn the_real_request_has_only_the_findings_the_fake_obs_explains() {
    let dir = tempfile::tempdir().unwrap();
    let report = review(&obs(), &options(dir.path(), true), &deps())
        .unwrap()
        .report;
    let found: Vec<_> = report.check_findings().map(|f| f.id).collect();
    // The fake holds no build result for the source package.
    assert_eq!(
        found,
        [ids::BUILD_NO_RESULTS],
        "{:?}",
        report.check_findings().collect::<Vec<_>>()
    );
    assert_eq!(report.exit, 1);
    let a = &report.actions[0];
    assert_eq!(a.old_version.as_deref(), Some("2.10.3"));
    assert_eq!(a.new_version.as_deref(), Some("2.10.4"));
    assert_eq!(a.added, ["cracklib-2.10.4.tar.bz2"]);
    assert_eq!(report.request.state, "new");
}

#[test]
fn no_download_never_downloads_and_says_what_was_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let o = obs();
    let report = review(&o, &options(dir.path(), false), &deps())
        .unwrap()
        .report;
    assert_eq!(o.called("download"), 0);
    assert!(
        report.actions[0]
            .skipped
            .iter()
            .any(|s| s.why.contains("--no-download"))
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn a_download_makes_one_call_per_wanted_file_and_nothing_else_is_written_to_obs() {
    let dir = tempfile::tempdir().unwrap();
    let o = obs();
    review(&o, &options(dir.path(), true), &deps()).unwrap();
    assert_eq!(o.called("download"), 1, "{:?}", o.calls.borrow());
    let writes = ["PUT", "DELETE", "commit", "create"];
    assert!(
        o.calls
            .borrow()
            .iter()
            .all(|c| !writes.iter().any(|w| c.contains(w)))
    );
}

#[test]
fn a_missing_request_is_an_error_and_a_threshold_of_blocker_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path(), false);
    opts.id = "42";
    assert!(review(&obs(), &opts, &deps()).is_err());
    let mut opts = options(dir.path(), false);
    opts.fail_on = Severity::Blocker;
    assert_eq!(review(&obs(), &opts, &deps()).unwrap().report.exit, 0);
}

#[test]
fn a_read_the_checks_need_ends_the_review_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    // The build results of the source cannot be read.
    let mut o = obs();
    o.failing
        .insert("results", super::fetch::ReadFailure::new("503"));
    let e = match review(&o, &options(dir.path(), false), &deps()) {
        Err(e) => e,
        Ok(_) => panic!("a review without build results was completed"),
    };
    assert!(e.0.contains("build results") && e.0.contains("503"), "{e}");
    // An optional read (the planner, the open requests) is only a note.
    let mut o = obs();
    o.failing
        .insert("open_requests_to", super::fetch::ReadFailure::new("503"));
    let report = review(&o, &options(dir.path(), false), &deps())
        .unwrap()
        .report;
    assert!(
        report
            .check_findings()
            .any(|f| f.id == ids::REVIEW_INCOMPLETE && f.severity == Severity::Note)
    );
}
