use crate::pipeline::review::carried_note;
use crate::pipeline::update_mode::*;

fn origin(version: &str) -> OriginBase {
    OriginBase {
        version: version.to_string(),
        spec: Some(format!("Name: foo\nVersion: {version}\n")),
    }
}

#[test]
fn a_kept_recipe_behind_its_origin_is_an_update_even_when_the_checkout_is_already_bumped() {
    // A fresh work dir of a branch an earlier run already bumped: the checkout
    // is at 2.0, the origin ships 1.0, and the run is still an update.
    let mode = update_mode(true, Some(&origin("1.0")), true).expect("an update");
    assert_eq!(
        mode.origin_spec.as_deref(),
        Some("Name: foo\nVersion: 1.0\n")
    );
}

#[test]
fn a_spec_generated_from_scratch_is_not_an_update_whatever_the_origin_ships() {
    // `--regen`, a wheel switch or a failed carry: the recipe is not the origin's.
    assert!(update_mode(true, Some(&origin("1.0")), false).is_none());
}

#[test]
fn a_run_that_is_not_an_update_is_never_one() {
    assert!(update_mode(false, Some(&origin("1.0")), true).is_none());
    assert!(update_mode(false, None, true).is_none());
}

#[test]
fn a_kept_recipe_whose_origin_is_at_the_target_keeps_the_origin_spec() {
    let mode = update_mode(true, Some(&origin("2.0")), true).expect("an update");
    assert_eq!(
        mode.origin_spec.as_deref(),
        Some("Name: foo\nVersion: 2.0\n")
    );
}

#[test]
fn a_kept_recipe_without_an_origin_is_an_update_without_an_origin_spec() {
    let mode = update_mode(true, None, true).expect("an update");
    assert!(mode.origin_spec.is_none());
}

#[test]
fn a_package_with_no_origin_and_no_kept_recipe_is_not_an_update() {
    assert!(update_mode(true, None, false).is_none());
}

#[test]
fn a_carried_note_says_what_this_run_changed() {
    let carried = carried_note(true, "1.0", "2.0");
    assert!(carried.contains("only `Version:` and the source changed"));
    let kept = carried_note(false, "1.0", "2.0");
    assert!(!kept.contains("only `Version:`"));
    assert!(kept.contains("already holds at 2.0"));
    assert!(kept.contains("the origin ships 1.0"));
}
