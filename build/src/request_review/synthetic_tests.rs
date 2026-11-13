use super::gather_tests::obs;
use super::ids;
use super::run::{Deps, Options};
use super::synthetic::review_branch;
use crate::update::{Candidate, Decision, Ecosystem};

const SP: &str = "home:jdoe:auto-package:branches:Base:System";
const TP: &str = "Base:System";

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
        forge: &crate::request_review::provenance_check::NO_FORGE,
    }
}

fn options<'a>(scratch: &'a std::path::Path, download: bool) -> Options<'a> {
    Options {
        id: "",
        download,
        scratch,
        fail_on: super::Severity::Warning,
        fail_on_agent: false,
        build: "test",
    }
}

#[test]
fn a_branch_review_judges_the_same_action_as_the_request_does() {
    let dir = tempfile::tempdir().unwrap();
    let report = review_branch(
        &obs(),
        SP,
        "cracklib",
        "3",
        TP,
        "cracklib",
        &options(dir.path(), true),
        &deps(),
    )
    .unwrap();
    // The same fixture as request 1394662: only the build has no result.
    let found: Vec<_> = report.check_findings().map(|f| f.id).collect();
    assert_eq!(found, [ids::BUILD_NO_RESULTS]);
    assert_eq!(report.actions.len(), 1);
    assert_eq!(report.actions[0].source.rev.as_deref(), Some("3"));
    assert_eq!(report.actions[0].target.project, TP);
    assert_eq!(report.actions[0].added, ["cracklib-2.10.4.tar.bz2"]);
    assert_eq!(report.request.id, "");
}

#[test]
fn a_branch_review_reads_only_and_never_asks_obs_for_a_request() {
    let dir = tempfile::tempdir().unwrap();
    let o = obs();
    review_branch(
        &o,
        SP,
        "cracklib",
        "3",
        TP,
        "cracklib",
        &options(dir.path(), false),
        &deps(),
    )
    .unwrap();
    // The document is the synthetic one: OBS's request is not read.
    assert_eq!(o.called("request"), 0);
    assert_eq!(o.called("download"), 0);
    const READS: [&str; 7] = [
        "listing",
        "file",
        "diff",
        "results",
        "open_requests_to",
        "current_srcmd5",
        "download",
    ];
    for call in o.calls.borrow().iter() {
        let name = call.split_whitespace().next().unwrap_or_default();
        assert!(READS.contains(&name), "unexpected read: {call}");
    }
}

#[test]
fn a_branch_that_is_not_there_is_an_error_and_no_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = obs();
    o.listings
        .remove(&(SP.into(), "cracklib".into(), Some("3".into())));
    let e = review_branch(
        &o,
        SP,
        "cracklib",
        "3",
        TP,
        "cracklib",
        &options(dir.path(), false),
        &deps(),
    )
    .unwrap_err();
    assert!(e.0.contains("no longer exists"), "{e}");
}
