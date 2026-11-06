use super::*;
use crate::{Cli, Commands};
use clap::Parser;

fn args(extra: &[&str]) -> UpdateProjectArgs {
    let mut argv = vec!["auto-package", "lint", "--project", "Base:System"];
    argv.extend_from_slice(extra);
    match Cli::try_parse_from(argv).unwrap().command {
        Commands::Lint(mut a) => {
            // `lint` sets it when it runs.
            a.improve = true;
            a
        }
        _ => panic!("not lint"),
    }
}

fn item(version: &str) -> Item {
    Item {
        pkg: "debugedit".into(),
        origin: "Base:System".into(),
        ecosystem: ap_build::update::Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: version.into(),
        new_version: version.into(),
        upstream_tag: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: Vec::new(),
    }
}

fn decided(
    a: &UpdateProjectArgs,
    origin: Result<&str, &str>,
    held: Result<Option<&str>, &str>,
) -> Continuation {
    decide(
        &item("5.3"),
        a,
        &|| origin.map(str::to_string).map_err(str::to_string),
        &|| held.map(|h| h.map(str::to_string)).map_err(str::to_string),
    )
}

#[test]
fn a_branch_ahead_of_the_origin_is_continued_from_at_its_version() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Ok(Some("5.4"))),
        Continuation::Branch {
            version: "5.4".into()
        }
    );
}

#[test]
fn a_snapshot_branch_newer_than_the_origin_is_continued_from() {
    assert_eq!(
        decided(&args(&[]), Ok("1.0"), Ok(Some("1.3+git20260101.abc123"))),
        Continuation::Branch {
            version: "1.3+git20260101.abc123".into()
        }
    );
}

#[test]
fn a_branch_at_the_origin_is_rebased_as_before() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Ok(Some("5.3"))),
        Continuation::Origin
    );
}

#[test]
fn a_branch_behind_the_origin_is_rebased_as_before() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Ok(Some("5.2"))),
        Continuation::Origin
    );
}

#[test]
fn a_package_the_branch_does_not_hold_is_branched_as_before() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Ok(None)),
        Continuation::Origin
    );
}

#[test]
fn an_origin_that_cannot_be_read_leaves_the_branch_alone() {
    assert_eq!(
        decided(&args(&[]), Err("boom"), Ok(Some("5.4"))),
        Continuation::Skip("the origin's copy could not be read: boom".into())
    );
}

#[test]
fn a_branch_that_cannot_be_read_is_left_alone() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Err("boom")),
        Continuation::Skip("its copy could not be read: boom".into())
    );
}

#[test]
fn a_branch_with_a_macro_version_is_left_alone() {
    assert_eq!(
        decided(&args(&[]), Ok("5.3"), Ok(Some("%{upstream}"))),
        Continuation::Skip(
            "its version %{upstream} cannot be compared with the origin's 5.3".into()
        )
    );
}

#[test]
fn in_place_is_unchanged() {
    let a = args(&["--in-place"]);
    let read = std::cell::Cell::new(0);
    let c = decide(&item("5.3"), &a, &|| Ok("5.3".into()), &|| {
        read.set(read.get() + 1);
        Ok(Some("5.4".into()))
    });
    assert_eq!(c, Continuation::Origin);
    assert_eq!(read.get(), 0);
}

#[test]
fn a_branch_that_is_continued_from_is_not_rebased() {
    let calls = std::cell::Cell::new(0);
    let run = || {
        calls.set(calls.get() + 1);
        Ok(osc::Rebased::Clean)
    };
    assert_eq!(
        items::rebase_unless_continuing(false, run).unwrap(),
        osc::Rebased::NotALink
    );
    assert_eq!(calls.get(), 0);
    assert_eq!(
        items::rebase_unless_continuing(true, run).unwrap(),
        osc::Rebased::Clean
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn a_branch_ahead_has_its_index_address_at_its_own_version() {
    let mut it = item("25.3");
    it.ecosystem = ap_build::update::Ecosystem::Index {
        url: "https://sourceforge.net/projects/eric-ide/files/eric7/25.3/eric7-25.3.tar.gz".into(),
    };
    continue_at(&mut it, "26.11".into());
    assert_eq!(
        it.ecosystem,
        ap_build::update::Ecosystem::Index {
            url: "https://sourceforge.net/projects/eric-ide/files/eric7/26.11/eric7-26.11.tar.gz"
                .into()
        }
    );
    assert_eq!(it.old_version, "26.11");
    assert_eq!(it.new_version, "26.11");
}

#[test]
fn a_git_source_names_no_version_and_is_kept() {
    let mut it = item("5.3");
    continue_at(&mut it, "5.4".into());
    assert_eq!(
        it.ecosystem,
        ap_build::update::Ecosystem::GitRepo {
            url: "https://github.com/a/b".into()
        }
    );
    assert_eq!(
        (it.old_version.as_str(), it.new_version.as_str()),
        ("5.4", "5.4")
    );
}
