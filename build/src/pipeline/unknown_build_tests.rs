//! An update of an archive whose build system auto-package does not know: the
//! recipe its checkout holds is carried forward, or the run fails with the
//! reason. No spec is generated for it.

use super::generate_stage::{UnknownBuild, keeps_recipe_on_failed_carry, unknown_build_gate};

#[test]
fn a_failed_carry_keeps_a_non_python_recipe_whatever_its_source() {
    assert!(keeps_recipe_on_failed_carry("fwupd-efi"));
    assert!(!keeps_recipe_on_failed_carry("python-requests"));
}

const ARCHIVE: &str = "https://github.com/project-repo/cagebreak/archive/refs/tags/3.2.1.tar.gz";

/// An update that has a spec, does not carry it forward and is not a wheel.
fn update(archive: &str) -> UnknownBuild<'_> {
    UnknownBuild {
        unknown: true,
        archive,
        version: "3.2.1",
        need_spec: true,
        spec_existed: true,
        wheel_route: false,
        carry: None,
    }
}

#[test]
fn a_failed_carry_fails_with_the_probe_error_and_the_reason() {
    let mut case = update(ARCHIVE);
    case.carry = Some(Err("the spec's sources have no web address"));
    let msg = unknown_build_gate(&case).unwrap_err();
    assert!(
        msg.starts_with(&format!("{ARCHIVE} contains no pyproject.toml")),
        "{msg}"
    );
    assert!(msg.contains("cannot tell how to build it"), "{msg}");
    assert!(
        msg.contains("built only from the package's own recipe"),
        "{msg}"
    );
    assert!(
        msg.ends_with(
            "could not be carried forward to 3.2.1 (the spec's sources have no web address)"
        ),
        "{msg}"
    );
}

#[test]
fn a_carried_recipe_lets_the_stage_go_on() {
    let mut case = update(ARCHIVE);
    case.carry = Some(Ok(()));
    assert_eq!(unknown_build_gate(&case), Ok(()));
}

#[test]
fn an_update_without_a_spec_has_no_recipe_to_carry_forward() {
    let mut case = update(ARCHIVE);
    case.spec_existed = false;
    let msg = unknown_build_gate(&case).unwrap_err();
    assert!(
        msg.ends_with("; there is no recipe to carry forward"),
        "{msg}"
    );
    // A wheel asked for without a spec has no recipe either.
    case.wheel_route = true;
    let msg = unknown_build_gate(&case).unwrap_err();
    assert!(
        msg.ends_with("; there is no recipe to carry forward"),
        "{msg}"
    );
}

#[test]
fn the_wheel_route_is_not_available_for_an_unknown_build_system() {
    // A spec exists and the wheel is asked for: the recipe is there, but a
    // wheel is not what it builds.
    let mut case = update(ARCHIVE);
    case.wheel_route = true;
    let msg = unknown_build_gate(&case).unwrap_err();
    assert!(
        msg.ends_with("; the wheel route is not available for an unknown build system"),
        "{msg}"
    );
    assert!(!msg.contains("no recipe to carry forward"), "{msg}");
}

#[test]
fn a_known_build_system_is_not_gated() {
    let mut case = update(ARCHIVE);
    case.unknown = false;
    for carry in [None, Some(Ok(())), Some(Err("no recipe"))] {
        for wheel_route in [false, true] {
            case.carry = carry;
            case.wheel_route = wheel_route;
            assert_eq!(unknown_build_gate(&case), Ok(()), "{carry:?} {wheel_route}");
        }
    }
    case.need_spec = false;
    case.carry = None;
    assert_eq!(unknown_build_gate(&case), Ok(()));
}

#[test]
fn nothing_is_gated_when_no_spec_is_generated() {
    let mut case = update(ARCHIVE);
    case.need_spec = false;
    assert_eq!(unknown_build_gate(&case), Ok(()));
}
