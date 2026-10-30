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

mod review_turn {
    use crate::osc::BuildStatus;
    use crate::pipeline::checkout::CheckoutFacts;
    use crate::pipeline::review_turn_context;
    use crate::pipeline::review_turn_wanted;

    const PKG: &str = "hyprlang";

    fn facts(obs: Option<BuildStatus>, local_changes: bool) -> CheckoutFacts {
        CheckoutFacts {
            obs_status: obs,
            local_changes,
        }
    }

    #[test]
    fn an_update_on_a_fresh_checkout_that_obs_built_is_not_reviewed() {
        // The checkout is already at the new version, the work dir is new, and OBS
        // succeeded on exactly that spec: no edit is wanted.
        let checkout = facts(Some(BuildStatus::Succeeded), false);
        assert!(!review_turn_wanted(false, true, &checkout, false, PKG));
    }

    #[test]
    fn an_update_that_obs_failed_is_reviewed_as_a_carried_recipe() {
        let checkout = facts(Some(BuildStatus::Failed), false);
        assert!(review_turn_wanted(false, true, &checkout, false, PKG));
    }

    #[test]
    fn an_update_whose_checkout_holds_unbuilt_edits_is_reviewed_even_if_obs_succeeded() {
        let checkout = facts(Some(BuildStatus::Succeeded), true);
        assert!(review_turn_wanted(false, true, &checkout, false, PKG));
    }

    #[test]
    fn a_succeeded_status_that_is_not_confirmed_for_the_checkout_is_not_exact() {
        // OBS answered `succeeded` for an earlier revision: that is not the spec on disk.
        assert!(!facts(Some(BuildStatus::Unknown("stale".into())), false).exact_success());
        assert!(facts(Some(BuildStatus::Succeeded), false).exact_success());
        assert!(!facts(None, false).exact_success());
    }

    #[test]
    fn a_new_spec_is_always_reviewed() {
        let checkout = facts(None, false);
        assert!(review_turn_wanted(true, false, &checkout, false, PKG));
    }

    #[test]
    fn a_reviewed_spec_or_a_cuda_redistributable_is_not_reviewed_again() {
        let checkout = facts(None, false);
        assert!(!review_turn_wanted(true, true, &checkout, true, PKG));
    }

    #[test]
    fn a_package_that_is_not_an_update_keeps_the_rule_for_a_failing_build() {
        let unresolvable = facts(Some(BuildStatus::Unresolvable), false);
        assert!(!review_turn_wanted(false, false, &unresolvable, false, PKG));
        assert!(review_turn_wanted(
            false,
            false,
            &facts(None, false),
            false,
            PKG
        ));
    }

    #[test]
    fn every_context_that_applies_is_given_to_the_turn() {
        assert_eq!(
            review_turn_context(Some("patch"), Some("submodules"), Some("carry")).as_deref(),
            Some("patch")
        );
        assert_eq!(
            review_turn_context(None, Some("submodules"), Some("carry")).as_deref(),
            Some("submodules\n\ncarry")
        );
        assert_eq!(
            review_turn_context(None, None, Some("carry")).as_deref(),
            Some("carry")
        );
        assert_eq!(review_turn_context(None, None, None), None);
    }
}

mod post_build_review {
    use crate::pipeline::build_round::post_review_ruled_out;

    #[test]
    fn an_update_is_not_reviewed_after_its_build() {
        assert!(post_review_ruled_out(false, true, false, 10.0));
    }

    #[test]
    fn a_new_package_keeps_its_post_build_review() {
        assert!(!post_review_ruled_out(false, false, false, 10.0));
    }

    #[test]
    fn the_review_still_stops_once_run_or_out_of_budget_or_without_agent() {
        assert!(post_review_ruled_out(true, false, false, 10.0));
        assert!(post_review_ruled_out(false, false, true, 10.0));
        assert!(post_review_ruled_out(false, false, false, 0.0));
    }
}
