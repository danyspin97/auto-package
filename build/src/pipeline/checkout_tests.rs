//! The version guards of the checkout: the branch check before the first turn,
//! and the `Version:` a turn or the time between turns may not change.

use crate::pipeline::checkout::{settle_branch_version, source_is_available, version_stop};
use std::cell::Cell;

fn spec(version: &str) -> String {
    format!(
        "Name: foo\nVersion: {version}\nRelease: 0\nSource0: https://example.org/foo-{version}.tar.gz\n"
    )
}

/// A checkout whose files are all listed.
fn all_listed(_: &str) -> bool {
    true
}

#[test]
fn a_checkout_at_the_branch_version_is_not_reread() {
    let text = spec("1.0");
    let reread = Cell::new(0);
    let r = settle_branch_version(
        || Some(text.clone()),
        "1.0",
        "2.0",
        || {
            reread.set(reread.get() + 1);
            Ok(())
        },
        all_listed,
    );
    assert!(r.is_ok());
    assert_eq!(reread.get(), 0);
}

#[test]
fn a_checkout_at_the_target_version_is_accepted_dirty_or_committed() {
    let r = settle_branch_version(|| Some(spec("2.0")), "1.0", "2.0", || Ok(()), all_listed);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn a_checkout_fixed_by_one_reread_is_accepted() {
    let reads = Cell::new(0);
    let r = settle_branch_version(
        || {
            reads.set(reads.get() + 1);
            Some(spec(if reads.get() == 1 { "3.0" } else { "1.0" }))
        },
        "1.0",
        "2.0",
        || Ok(()),
        all_listed,
    );
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(reads.get(), 2);
}

#[test]
fn a_third_version_stops_the_package_after_one_reread() {
    let reread = Cell::new(0);
    let r = settle_branch_version(
        || Some(spec("3.0")),
        "1.0",
        "2.0",
        || {
            reread.set(reread.get() + 1);
            Ok(())
        },
        all_listed,
    );
    let why = r.unwrap_err();
    assert_eq!(reread.get(), 1, "the reread runs once");
    assert!(
        why.contains("Version 3.0") && why.contains("expects 1.0"),
        "{why}"
    );
}

#[test]
fn a_source_the_package_does_not_have_stops_the_package() {
    let r = settle_branch_version(
        || Some(spec("1.0")),
        "1.0",
        "2.0",
        || Ok(()),
        |file| file != "foo-1.0.tar.gz",
    );
    let why = r.unwrap_err();
    assert!(why.contains("Source foo-1.0.tar.gz"), "{why}");
}

#[test]
fn a_missing_spec_or_a_failed_reread_is_a_mismatch() {
    let r = settle_branch_version(|| None, "1.0", "2.0", || Ok(()), all_listed);
    assert!(r.is_err());
    let r = settle_branch_version(
        || Some(spec("3.0")),
        "1.0",
        "2.0",
        || Err("osc up failed".into()),
        all_listed,
    );
    let why = r.unwrap_err();
    assert!(why.contains("osc up failed"), "{why}");
}

#[test]
fn a_spec_at_the_package_version_passes_the_turn_check() {
    let text = spec("2.0");
    assert!(version_stop(&text, "2.0", "before the turn").is_none());
}

#[test]
fn a_version_reset_between_turns_stops_the_package() {
    // The spec was 2.0 after the last turn; something outside the agent set 1.0.
    let why = version_stop(&spec("1.0"), "2.0", "before the turn").expect("a stop");
    assert!(
        why.contains("Version is 1.0") && why.contains("package is stopped"),
        "{why}"
    );
}

#[test]
fn an_edited_turn_that_changes_the_version_stops_the_package() {
    // A turn's edit leaves the spec at the old version: the check after the
    // turn stops it, whatever the action, and the caller restores the spec.
    let why = version_stop(&spec("1.0"), "2.0", "after the turn").expect("a stop");
    assert!(why.starts_with("after the turn"), "{why}");
}

#[test]
fn a_turn_that_keeps_the_version_is_not_a_stop() {
    assert!(version_stop(&spec("2.0"), "2.0", "after the turn").is_none());
    assert!(version_stop("no version here", "2.0", "after the turn").is_none());
}

#[test]
fn a_source_a_service_makes_is_a_file_of_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    let head = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    // Listed, plainly or as a server-side service's output.
    assert!(source_is_available(
        "foo-1.0.tar.gz",
        &head(&["foo-1.0.tar.gz"]),
        checkout
    ));
    assert!(source_is_available(
        "foo-1.0.tar.gz",
        &head(&["_service:download_url:foo-1.0.tar.gz", "foo.spec"]),
        checkout
    ));
    // The checkout holds the service's output under its `_service:` name.
    std::fs::write(checkout.join("_service:tar_scm:bar-2.tar.xz"), b"x").unwrap();
    assert!(source_is_available("bar-2.tar.xz", &[], checkout));
    // Not listed, not there, no service: a source the package does not have.
    assert!(!source_is_available(
        "baz-3.tar.gz",
        &head(&["foo.spec"]),
        checkout
    ));
    // A download service in the checkout's `_service` fetches it at build time.
    std::fs::write(
        checkout.join("_service"),
        "<services><service name=\"download_files\" mode=\"buildtime\"/></services>\n",
    )
    .unwrap();
    assert!(source_is_available(
        "baz-3.tar.gz",
        &head(&["foo.spec"]),
        checkout
    ));
    // A `_service` that runs nothing of the kind does not.
    std::fs::write(
        checkout.join("_service"),
        "<services><service name=\"set_version\"/></services>\n",
    )
    .unwrap();
    assert!(!source_is_available(
        "baz-3.tar.gz",
        &head(&["foo.spec"]),
        checkout
    ));
}

/// PyPI's spelling of a sdist is a case match for the spec's `Source`: the
/// checkout is accepted and `align_source_case` renames the file afterwards.
#[test]
fn a_source_the_checkout_holds_in_another_case_is_available() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("amqpstorm-3.1.4.tar.gz"), "x").unwrap();
    assert!(source_is_available(
        "AMQPStorm-3.1.4.tar.gz",
        &[],
        dir.path()
    ));
    assert!(!source_is_available("other-3.1.4.tar.gz", &[], dir.path()));
}
