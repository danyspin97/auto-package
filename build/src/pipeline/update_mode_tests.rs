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
        assert!(!review_turn_wanted(
            false, true, &checkout, false, false, PKG
        ));
    }

    #[test]
    fn an_update_that_obs_failed_is_reviewed_as_a_carried_recipe() {
        let checkout = facts(Some(BuildStatus::Failed), false);
        assert!(review_turn_wanted(
            false, true, &checkout, false, false, PKG
        ));
    }

    #[test]
    fn an_update_whose_checkout_holds_unbuilt_edits_is_reviewed_even_if_obs_succeeded() {
        let checkout = facts(Some(BuildStatus::Succeeded), true);
        assert!(review_turn_wanted(
            false, true, &checkout, false, false, PKG
        ));
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
        assert!(review_turn_wanted(
            true, false, &checkout, false, false, PKG
        ));
    }

    #[test]
    fn a_reviewed_spec_or_a_cuda_redistributable_is_not_reviewed_again() {
        let checkout = facts(None, false);
        assert!(!review_turn_wanted(true, true, &checkout, false, true, PKG));
    }

    #[test]
    fn a_failed_build_with_its_log_is_fixed_by_the_pre_fix_turn_not_reviewed() {
        let unresolvable = facts(Some(BuildStatus::Unresolvable), false);
        assert!(!review_turn_wanted(
            false,
            false,
            &unresolvable,
            true,
            false,
            PKG
        ));
    }

    #[test]
    fn a_failed_build_without_a_log_is_still_reviewed() {
        // OBS failed it, but there is no log for the pre-fix turn to work from.
        let failed = facts(Some(BuildStatus::Failed), false);
        assert!(review_turn_wanted(false, false, &failed, false, false, PKG));
        assert!(review_turn_wanted(
            false,
            false,
            &facts(None, false),
            false,
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
        assert!(post_review_ruled_out(false, true, false, false));
    }

    #[test]
    fn a_new_package_keeps_its_post_build_review() {
        assert!(!post_review_ruled_out(false, false, false, false));
    }

    #[test]
    fn the_review_still_stops_once_run_or_out_of_budget_or_without_agent() {
        assert!(post_review_ruled_out(true, false, false, false));
        assert!(post_review_ruled_out(false, false, true, false));
        assert!(post_review_ruled_out(false, false, false, true));
    }
}

mod inherited_checks {
    use crate::pipeline::update_mode::{newly_reported, split_inherited, style_problems};
    use std::collections::BTreeSet;

    /// A summary-only description and no `%license`, as an origin ships it.
    const SUMMARY_ONLY: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool for things\n\
        License: MIT\n\n%description\nA tool for things\n\n%build\n\n%files\n%{_bindir}/foo\n\n\
        %changelog\n";

    /// The same recipe with a sentence of description and a `%license`.
    const DESCRIBED: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: A tool for things\n\
        License: MIT\n\n%description\nA tool for things. It runs the jobs of a build.\n\n\
        %build\n\n%files\n%license LICENSE\n%{_bindir}/foo\n\n%changelog\n";

    fn source_with_licence() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("LICENSE"), "MIT\n").unwrap();
        dir
    }

    #[test]
    fn an_inherited_summary_only_description_without_license_is_left_on_an_update() {
        let src = source_with_licence();
        let found = style_problems(SUMMARY_ONLY, src.path());
        assert_eq!(
            found.len(),
            2,
            "the description and the %license: {found:?}"
        );
        let origin = style_problems(SUMMARY_ONLY, src.path());
        let (introduced, inherited) = split_inherited(found, &origin);
        assert!(introduced.is_empty());
        assert_eq!(inherited.len(), 2);
    }

    #[test]
    fn the_same_problems_fail_a_new_spec() {
        let src = source_with_licence();
        let found = style_problems(SUMMARY_ONLY, src.path());
        let (introduced, inherited) = split_inherited(found.clone(), &[]);
        assert_eq!(introduced, found);
        assert!(inherited.is_empty());
    }

    #[test]
    fn a_problem_this_version_introduces_is_still_fixed_on_an_update() {
        let src = source_with_licence();
        let origin = style_problems(DESCRIBED, src.path());
        assert!(origin.is_empty(), "the origin is clean: {origin:?}");
        let found = style_problems(SUMMARY_ONLY, src.path());
        let (introduced, inherited) = split_inherited(found, &origin);
        assert_eq!(introduced.len(), 2);
        assert!(inherited.is_empty());
    }

    #[test]
    fn an_emoji_the_origin_does_not_have_is_introduced_even_if_it_has_another() {
        let src = source_with_licence();
        let origin = DESCRIBED.replace("a build.", &format!("a build {}", '\u{1F680}'));
        let new = DESCRIBED.replace("a build.", &format!("a build {}", '\u{1F389}'));
        let origin_problems = style_problems(&origin, src.path());
        assert_eq!(origin_problems.len(), 1, "{origin_problems:?}");
        let (introduced, inherited) =
            split_inherited(style_problems(&new, src.path()), &origin_problems);
        assert_eq!(introduced.len(), 1, "{introduced:?}");
        assert!(inherited.is_empty());
    }

    #[test]
    fn an_inherited_problem_is_reported_once_per_run() {
        let mut reported = BTreeSet::new();
        let first = newly_reported(&mut reported, vec!["a".into(), "b".into()]);
        assert_eq!(first, vec!["a".to_string(), "b".to_string()]);
        assert!(newly_reported(&mut reported, vec!["a".into()]).is_empty());
        assert_eq!(
            newly_reported(&mut reported, vec!["c".into()]),
            vec!["c".to_string()]
        );
    }
}

mod tidy_on_update {
    use crate::pipeline::update_mode::normalize_spec_text;

    /// `%check` after `%files`: the order `move_check_before_files` puts right.
    const CHECK_AFTER_FILES: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool\n\
        License: MIT\nSource0: foo-2.0.tar.gz\n\n%description\nA tool that does things.\n\n\
        %prep\n%setup -q\n\n%build\n\n%install\n\n%files\n%{_bindir}/foo\n\n%check\nmake test\n\n\
        %changelog\n";

    /// A `Requires` written after `%python_subpackages`.
    const SUBPACKAGE_REQUIRES: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool\n\
        License: MIT\n\n%description\nA tool that does things.\n\n%python_subpackages\n\
        Requires: python-bar\n\n%prep\n%setup -q\n\n%build\n\n%install\n\n%files\n\
        %{_bindir}/foo\n\n%changelog\n";

    /// A pytest BuildRequires that a `%check` running an import does not use.
    const UNUSED_PYTEST: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool\n\
        License: MIT\nSource0: foo-2.0.tar.gz\nBuildRequires: %{python_module pytest}\n\n\
        %description\nA tool that does things.\n\n%prep\n%setup -q\n\n%build\n\n%install\n\n\
        %check\n%python_expand $python -c 'import foo'\n\n%files\n%{_bindir}/foo\n\n\
        %changelog\n";

    #[test]
    fn an_update_keeps_the_order_the_origin_chose() {
        // Only the version macro changes (see the test below): the order stays.
        let kept = normalize_spec_text(CHECK_AFTER_FILES, true);
        assert_eq!(
            kept,
            CHECK_AFTER_FILES.replace("foo-2.0.tar.gz", "foo-%{version}.tar.gz")
        );
    }

    #[test]
    fn a_new_spec_still_gets_the_section_order() {
        let moved = normalize_spec_text(CHECK_AFTER_FILES, false);
        assert_ne!(moved, CHECK_AFTER_FILES);
        let check = moved.find("\n%check").expect("a %check");
        let files = moved.find("\n%files").expect("a %files");
        assert!(check < files, "%check moved before %files:\n{moved}");
    }

    #[test]
    fn an_update_fixes_a_requires_written_after_the_subpackages_it_belongs_to() {
        // A dependency tag after `%python_subpackages` attaches to the wrong
        // subpackage: an edit of this version is fixed on an update too.
        let kept = normalize_spec_text(SUBPACKAGE_REQUIRES, true);
        let tag = kept.find("Requires: python-bar").expect("the tag");
        let sub = kept.find("%python_subpackages").expect("the subpackages");
        assert!(tag < sub, "{kept}");
    }

    #[test]
    fn an_update_drops_a_pytest_build_requires_its_check_does_not_use() {
        let kept = normalize_spec_text(UNUSED_PYTEST, true);
        assert!(!kept.contains("%{python_module pytest}"), "{kept}");
    }

    /// The obsolete constructs `lint` removes from an existing spec, all in one.
    const OBSOLETE: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool\n\
        License: MIT\nGroup: Development/Tools\nBuildRoot: %{_tmppath}/%{name}\n\
        Source0: foo-2.0.tar.gz\n\n%description\nA tool that does things.\n\n\
        %prep\n%setup -q\n\n%build\n\n%install\nrm -rf %{buildroot}\nmake install\n\n\
        %post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig\n\n\
        %files\n%defattr(-,root,root)\n%{_bindir}/foo\n\n\
        %clean\nrm -rf %{buildroot}\n\n%changelog\n";

    #[test]
    fn an_update_leaves_the_obsolete_constructs_for_lint() {
        // The hygiene passes of `lint` are not in the update's normalisation:
        // an update keeps the maintainer's recipe (convention 86).
        let kept = normalize_spec_text(OBSOLETE, true);
        for construct in [
            "Group: Development/Tools",
            "BuildRoot: %{_tmppath}/%{name}",
            "rm -rf %{buildroot}\nmake install",
            "%post -p /sbin/ldconfig\n%postun -p /sbin/ldconfig",
            "%defattr(-,root,root)",
            "%clean\nrm -rf %{buildroot}",
        ] {
            assert!(kept.contains(construct), "{construct} was removed:\n{kept}");
        }
        assert_eq!(
            kept,
            OBSOLETE.replace("foo-2.0.tar.gz", "foo-%{version}.tar.gz")
        );
    }
}

mod failed_build_log {
    use crate::osc::BuildStatus;
    use crate::pipeline::checkout::CheckoutFacts;

    fn facts(obs: Option<BuildStatus>) -> CheckoutFacts {
        CheckoutFacts {
            obs_status: obs,
            local_changes: false,
        }
    }

    #[test]
    fn a_log_of_a_failed_or_rejected_build_on_obs_is_the_failed_log() {
        assert!(facts(Some(BuildStatus::Failed)).has_failed_build_log(true));
        assert!(facts(Some(BuildStatus::Unresolvable)).has_failed_build_log(true));
    }

    #[test]
    fn a_log_left_by_an_earlier_build_is_not_one_when_obs_succeeded_or_answered_nothing() {
        assert!(!facts(Some(BuildStatus::Succeeded)).has_failed_build_log(true));
        assert!(!facts(None).has_failed_build_log(true));
        assert!(!facts(Some(BuildStatus::Failed)).has_failed_build_log(false));
    }
}
