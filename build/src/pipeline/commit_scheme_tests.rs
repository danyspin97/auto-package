//! The version scheme the commit step's leftover cleanup compares in.

use super::commit::leftover_scheme;
use super::hygiene_tests::config;
use crate::version::{Scheme, cmp};
use std::cmp::Ordering;

#[test]
fn the_commit_step_uses_the_scheme_of_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut pypi = config("foo", dir.path());
    pypi.scheme = Scheme::Pep440;
    assert_eq!(leftover_scheme(&pypi), Some(Scheme::Pep440));
    // In PEP 440 a release candidate sorts before its release, which is the
    // order the cleanup needs for a leftover `1.0rc1` against the spec's `1.0`.
    assert_eq!(cmp("1.0rc1", "1.0", Scheme::Pep440), Some(Ordering::Less));
}

#[test]
fn a_run_without_a_known_ecosystem_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let plain = config("foo", dir.path());
    assert_eq!(leftover_scheme(&plain), Some(Scheme::Generic));
}
