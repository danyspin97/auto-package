use super::*;
use crate::{Cli, Commands};
use clap::Parser;
use std::cell::{Cell, RefCell};

fn args(extra: &[&str]) -> UpdateProjectArgs {
    let mut argv = vec!["auto-package", "update", "--project", "Base:System"];
    argv.extend_from_slice(extra);
    match Cli::try_parse_from(argv).unwrap().command {
        Commands::Update(a) => a,
        _ => panic!("not update"),
    }
}

fn item(old: &str, new: &str) -> Item {
    Item {
        pkg: "foo".into(),
        origin: "Base:System".into(),
        ecosystem: Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: old.into(),
        new_version: new.into(),
        upstream_tag: None,
        dependency_of: None,
        findings: Vec::new(),
    }
}

/// One request document: `state`, a source project and the actions given.
fn request(id: &str, state: &str, source: &str, action: &str) -> String {
    format!(
        r#"<request id="{id}" creator="alice"><action type="{action}">
        <source project="{source}" package="foo" rev="2"/>
        <target project="Base:System" package="foo"/></action>
        <state name="{state}"/></request>"#
    )
}

fn parse(requests: &[String]) -> Vec<osc::OpenRequest> {
    osc::parse_open_requests(&format!("<collection>{}</collection>", requests.concat()))
}

/// The verdict for `requests` where request N brings `versions[N]`.
fn verdict(it: &Item, a: &UpdateProjectArgs, requests: &[String], versions: &[&str]) -> Verdict {
    let all = parse(requests);
    decide(it, a, Ok(&all), &|r| {
        let n: usize = r.id.parse().unwrap();
        Ok(versions[n].to_string())
    })
}

const BRANCH: &str = "home:alice:branches:Base:System";

#[test]
fn a_request_with_the_target_version_skips() {
    let v = verdict(
        &item("1.0", "1.1"),
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    let found = v.skip.unwrap();
    assert_eq!(
        found.to_string(),
        "request 0 by alice brings 1.1 to foo: not duplicating the work"
    );
}

#[test]
fn a_request_with_a_newer_version_skips() {
    let v = verdict(
        &item("1.0", "1.1"),
        &args(&[]),
        &[request("0", "review", "devel:x", "submit")],
        &["1.2"],
    );
    assert_eq!(v.skip.unwrap().version, "1.2");
}

#[test]
fn a_request_with_an_older_version_is_noted_and_does_not_skip() {
    let v = verdict(
        &item("1.0", "1.2"),
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    assert_eq!(v.skip, None);
    assert_eq!(v.notes, ["request 0 brings 1.1, newer target 1.2"]);
}

#[test]
fn no_request_means_no_skip_and_no_note() {
    assert_eq!(
        verdict(&item("1.0", "1.1"), &args(&[]), &[], &[]),
        Verdict::default()
    );
}

#[test]
fn a_failed_read_of_the_requests_never_skips() {
    let v = decide(&item("1.0", "1.1"), &args(&[]), Err("timeout"), &|_| {
        Ok("1.1".into())
    });
    assert_eq!(v, Verdict::default());
}

#[test]
fn a_request_whose_version_cannot_be_read_does_not_skip() {
    let all = parse(&[request("0", "new", BRANCH, "submit")]);
    let v = decide(&item("1.0", "1.1"), &args(&[]), Ok(&all), &|_| {
        Err("no spec".into())
    });
    assert_eq!(v.skip, None);
    assert_eq!(v.notes.len(), 1);
    assert!(v.notes[0].contains("no spec"));
}

#[test]
fn a_declined_revoked_or_superseded_request_is_ignored() {
    for state in ["declined", "revoked", "superseded", "accepted"] {
        let v = verdict(
            &item("1.0", "1.1"),
            &args(&[]),
            &[request("0", state, BRANCH, "submit")],
            &["1.1"],
        );
        assert_eq!(v, Verdict::default(), "{state}");
    }
}

#[test]
fn a_request_that_is_not_a_submit_is_ignored() {
    for action in ["delete", "change_devel"] {
        let v = verdict(
            &item("1.0", "1.1"),
            &args(&[]),
            &[request("0", "new", BRANCH, action)],
            &["1.1"],
        );
        assert_eq!(v, Verdict::default(), "{action}");
    }
}

#[test]
fn a_snapshot_does_not_bring_a_release() {
    let v = verdict(
        &item("0.9", "1.0"),
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.0+git20260101"],
    );
    assert_eq!(v.skip, None);
    assert_eq!(
        v.notes,
        ["request 0 brings the snapshot 1.0+git20260101, not the release 1.0"]
    );
    // The same string as the target is the release itself.
    let same = verdict(
        &item("0.9", "1.0"),
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.0"],
    );
    assert_eq!(same.skip.unwrap().version, "1.0");
}

#[test]
fn a_request_to_another_target_project_with_the_same_package_is_ignored() {
    let other = r#"<request id="0" creator="alice"><action type="submit">
        <source project="home:alice" package="foo" rev="2"/>
        <target project="openSUSE:Factory" package="foo"/></action>
        <state name="new"/></request>"#
        .to_string();
    let v = verdict(&item("1.0", "1.1"), &args(&[]), &[other], &["1.1"]);
    assert_eq!(v, Verdict::default());
}

#[test]
fn one_request_with_two_submit_actions_is_judged_per_action() {
    let two = r#"<request id="0" creator="alice">
        <action type="submit"><source project="home:alice:a" package="foo" rev="2"/>
        <target project="Base:System" package="foo"/></action>
        <action type="submit"><source project="home:alice:b" package="foo" rev="3"/>
        <target project="Base:System" package="foo"/></action>
        <state name="new"/></request>"#
        .to_string();
    let all = parse(&[two]);
    assert_eq!(all.len(), 2);
    let v = decide(&item("1.0", "1.1"), &args(&[]), Ok(&all), &|r| {
        Ok(if r.source_project.ends_with(":a") {
            "1.0"
        } else {
            "1.1"
        }
        .to_string())
    });
    let found = v.skip.unwrap();
    assert_eq!((found.id.as_str(), found.version.as_str()), ("0", "1.1"));
    assert_eq!(v.notes, ["request 0 brings 1.0, newer target 1.1"]);
}

#[test]
fn a_skip_is_made_only_for_a_request_still_open() {
    // The kept list says `new`; the request read now says `declined`.
    let kept = verdict(
        &item("1.0", "1.1"),
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    assert!(kept.skip.is_some());
    let now = |_: &str| Ok(request("0", "declined", BRANCH, "submit"));
    let confirmed = confirm_open("0", &now);
    assert!(confirmed.unwrap_err().contains("declined"));
    let still = |_: &str| Ok(request("0", "review", BRANCH, "submit"));
    assert_eq!(confirm_open("0", &still), Ok(()));
    let failing = |_: &str| Err("503".to_string());
    assert!(confirm_open("0", &failing).unwrap_err().contains("503"));
}

#[test]
fn a_spec_version_is_cleaned_and_a_huge_spec_is_refused() {
    let spec = "Name: foo\nVersion: 1.\x1b[2J0\nRelease: 1\n";
    let v = spec_version(spec).unwrap();
    assert!(!v.contains('\x1b'), "{v:?}");
    let huge = format!("Name: foo\nVersion: 1.0\n{}", "#".repeat(SPEC_CAP));
    assert!(spec_version(&huge).unwrap_err().contains("larger"));
}

#[test]
fn a_request_from_the_users_own_branch_project_counts() {
    let v = verdict(
        &item("1.0", "1.1"),
        &args(&["--branch-project", BRANCH]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    assert!(v.skip.is_some());
}

#[test]
fn any_matching_request_among_several_skips() {
    let v = verdict(
        &item("1.0", "1.2"),
        &args(&[]),
        &[
            request("0", "new", BRANCH, "submit"),
            request("1", "declined", "devel:x", "submit"),
            request("2", "review", "devel:y", "submit"),
        ],
        &["1.1", "1.2", "1.3"],
    );
    assert_eq!(v.skip.unwrap().id, "2");
}

#[test]
fn a_request_to_another_package_is_ignored() {
    let mut other = item("1.0", "1.1");
    other.pkg = "bar".into();
    let v = verdict(
        &other,
        &args(&[]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    assert_eq!(v, Verdict::default());
}

#[test]
fn include_requested_turns_the_check_off() {
    let v = verdict(
        &item("1.0", "1.1"),
        &args(&["--include-requested"]),
        &[request("0", "new", BRANCH, "submit")],
        &["1.1"],
    );
    assert_eq!(v, Verdict::default());
}

#[test]
fn the_check_is_for_an_upgrade_of_a_plain_update_only() {
    let it = item("1.0", "1.1");
    assert!(applies(&it, &args(&[])));
    assert!(!applies(&item("1.0", "1.0"), &args(&[])), "lint item");
    assert!(!applies(&it, &args(&["--in-place"])));
    assert!(!applies(
        &it,
        &args(&["--maintenance", "SUSE:Maintenance:1"])
    ));
    assert!(!applies(&it, &args(&["--maintenance-list"])));
}

#[test]
fn the_requests_to_an_origin_are_read_once_and_grouped_by_package() {
    let reads = Cell::new(0);
    let read = |_: &str| {
        reads.set(reads.get() + 1);
        Ok(parse(&[request("0", "new", BRANCH, "submit")]))
    };
    let mut open = OpenRequests::default();
    let warn = |_: String| panic!("no warning expected");
    assert_eq!(
        open.to_package("Base:System", "foo", &read, &warn)
            .unwrap()
            .len(),
        1
    );
    assert!(
        open.to_package("Base:System", "bar", &read, &warn)
            .unwrap()
            .is_empty()
    );
    assert_eq!(reads.get(), 1);
}

#[test]
fn a_failed_read_warns_once_and_is_not_retried() {
    let reads = Cell::new(0);
    let read = |_: &str| {
        reads.set(reads.get() + 1);
        Err("503".to_string())
    };
    let warnings = RefCell::new(Vec::new());
    let warn = |w: String| warnings.borrow_mut().push(w);
    let mut open = OpenRequests::default();
    assert!(
        open.to_package("Base:System", "foo", &read, &warn)
            .is_none()
    );
    assert!(
        open.to_package("Base:System", "bar", &read, &warn)
            .is_none()
    );
    assert_eq!(reads.get(), 1);
    assert_eq!(warnings.borrow().len(), 1);
    assert!(warnings.borrow()[0].contains("503"));
}
