use super::*;
use crate::{Cli, Commands};
use clap::Parser;
use std::cell::Cell;

/// `update --project Base:System` with `extra` arguments.
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
        ecosystem: ap_build::update::Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: old.into(),
        new_version: new.into(),
        upstream_tag: None,
        dependency_of: None,
        findings: Vec::new(),
    }
}

/// The settlement for a branch holding `held` whose build reads `built`, and
/// how many times the build was read.
fn settled(
    it: &Item,
    a: &UpdateProjectArgs,
    held: Result<Option<&str>, &str>,
    built: Result<osc::BuildStatus, &str>,
) -> (Settlement, usize) {
    let reads = Cell::new(0);
    let s = settle(
        it,
        a,
        &|| held.map(|h| h.map(str::to_string)).map_err(str::to_string),
        &|| {
            reads.set(reads.get() + 1);
            built.clone().map_err(str::to_string)
        },
    );
    (s, reads.get())
}

#[test]
fn a_branch_at_upstream_that_built_is_updated_after_one_read() {
    let (s, reads) = settled(
        &item("1.0", "1.1"),
        &args(&[]),
        Ok(Some("1.1")),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Updated);
    assert_eq!(reads, 1);
}

#[test]
fn a_branch_at_upstream_that_did_not_build_is_continued_from() {
    for status in [
        osc::BuildStatus::Failed,
        osc::BuildStatus::Building,
        osc::BuildStatus::Unknown("no results".into()),
    ] {
        let (s, _) = settled(
            &item("1.0", "1.1"),
            &args(&[]),
            Ok(Some("1.1")),
            Ok(status.clone()),
        );
        assert_eq!(s, Settlement::Go, "{status}");
    }
}

#[test]
fn a_branch_whose_build_cannot_be_read_is_left_alone() {
    let (s, _) = settled(
        &item("1.0", "1.1"),
        &args(&[]),
        Ok(Some("1.1")),
        Err("timeout"),
    );
    assert!(matches!(s, Settlement::Unreadable(w) if w.contains("timeout")));
}

#[test]
fn the_build_is_read_only_for_a_branch_at_upstream() {
    for held in [None, Some("1.0"), Some("1.3")] {
        let (_, reads) = settled(
            &item("1.0", "1.2"),
            &args(&[]),
            Ok(held),
            Ok(osc::BuildStatus::Succeeded),
        );
        assert_eq!(reads, 0, "{held:?}");
    }
}

#[test]
fn a_lint_item_is_not_settled_by_a_branch_holding_its_version() {
    // Lint items have the same old and new version, and a branch that holds
    // the origin's version is no sign that the findings were fixed.
    let it = item("1.0", "1.0");
    let (s, reads) = settled(
        &it,
        &args(&[]),
        Ok(Some("1.0")),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Go);
    assert_eq!(reads, 0);
    let mut improve = args(&[]);
    improve.improve = true;
    let (s, _) = settled(
        &item("1.0", "1.1"),
        &improve,
        Ok(Some("1.1")),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Go);
}

#[test]
fn a_branch_ahead_of_upstream_is_listed_unless_the_target_is_forced() {
    let ahead = || Ok(Some("1.3"));
    let (s, _) = settled(
        &item("1.0", "1.2"),
        &args(&[]),
        ahead(),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(
        s,
        Settlement::Ahead {
            branch: "1.3".into()
        }
    );
    let (s, _) = settled(
        &item("1.0", "1.2"),
        &args(&["--only", "foo", "--to-version", "1.2"]),
        ahead(),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Go);
}

#[test]
fn a_revision_pin_is_not_settled_by_the_version() {
    let (s, _) = settled(
        &item("1.0", "1.1"),
        &args(&["--only", "foo", "--revision", "7"]),
        Ok(Some("1.1")),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Go);
}

#[test]
fn a_copy_that_cannot_be_read_is_left_alone() {
    let (s, _) = settled(
        &item("1.0", "1.1"),
        &args(&[]),
        Err("its spec has no readable Name: and Version:"),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert!(matches!(s, Settlement::Unreadable(w) if w.contains("no readable")));
}

#[test]
fn a_package_the_branch_does_not_hold_is_branched() {
    let (s, _) = settled(
        &item("1.0", "1.1"),
        &args(&[]),
        Ok(None),
        Ok(osc::BuildStatus::Succeeded),
    );
    assert_eq!(s, Settlement::Go);
}
