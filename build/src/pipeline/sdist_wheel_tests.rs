//! The sdist-to-wheel gate (convention 32): an update may not switch `Source0`
//! from the release's sdist to its wheel; a new package, a wheel to a wheel and a
//! package that asked for its wheel are not refused.

use super::*;

const SDIST: &str = "Name: python-hypothesis\nVersion: 6.152.6\n\
    Source0: https://files.pythonhosted.org/packages/x/hypothesis-%{version}.tar.gz\n\
    Source99: python-hypothesis-rpmlintrc\n";

const WHEEL: &str = "Name: python-hypothesis\nVersion: 6.170.15\n\
    Source0: https://files.pythonhosted.org/packages/x/hypothesis-6.170.15-cp311-abi3-manylinux2014_x86_64.manylinux_2_17_x86_64.whl\n";

#[test]
fn an_update_from_the_sdist_to_the_wheel_is_a_switch() {
    assert!(sdist_to_wheel(SDIST, WHEEL));
    let why = switch_refusal(true, SDIST, WHEEL).expect("refused");
    assert!(why.contains("convention 32"), "{why}");
    assert!(why.contains("use_upstream_wheel"), "{why}");
    assert!(why.contains("tar.gz"), "{why}");
}

#[test]
fn a_new_package_or_one_that_asked_for_its_wheel_is_not_refused() {
    // A new package has no Source0 before the edit.
    assert!(!sdist_to_wheel("", WHEEL));
    assert_eq!(switch_refusal(true, "", WHEEL), None);
    // A package whose wheel policy is set does not apply the gate.
    assert_eq!(switch_refusal(false, SDIST, WHEEL), None);
}

#[test]
fn a_wheel_to_a_wheel_and_an_sdist_kept_are_fine() {
    assert_eq!(switch_refusal(true, WHEEL, WHEEL), None);
    let newer = SDIST.replace("6.152.6", "6.170.15");
    assert_eq!(switch_refusal(true, SDIST, &newer), None);
}

#[test]
fn a_source_other_than_source0_does_not_count() {
    // The sdist stays Source0; the wheel is an extra source.
    let extra = format!("{SDIST}Source1: extra-1.0-py3-none-any.whl\n");
    assert!(!sdist_to_wheel(SDIST, &extra));
    // A comment naming Source0 is not the declaration.
    let commented = "# Source0: a-1.whl\n".to_string() + SDIST;
    assert_eq!(main_source(&commented), main_source(SDIST));
    assert_eq!(main_source("# Source0: a.whl\n"), None);
}

#[test]
fn the_plain_source_tag_is_source0() {
    assert_eq!(main_source("Source: a-1.tar.xz\n"), Some("a-1.tar.xz"));
    assert_eq!(main_source("source0:   a-1.zip  # note\n"), Some("a-1.zip"));
}
