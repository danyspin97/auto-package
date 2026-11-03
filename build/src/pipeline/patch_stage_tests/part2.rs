//! The rounds of the patch stage: the round bound, the offset refresh before a
//! turn, the gates, and the quilt series a refresh turn reads.

use super::*;
use crate::pipeline::removal_proof::*;

#[test]
fn the_rounds_are_bounded() {
    let scripts: Vec<Script> = (0..crate::patch_check::MAX_ROUNDS)
        .map(|i| -> Script {
            Box::new(move |checkout, _| {
                // A change every round, but never one that applies.
                std::fs::write(checkout.join("b.patch"), format!("try {i}")).unwrap();
                outcome(crate::agent::Action::Edited, "Refresh b.patch")
            })
        })
        .collect();
    let case = run(&[("b.patch", "old")], false, 10, scripts);
    assert_eq!(case.contexts.len(), crate::patch_check::MAX_ROUNDS);
    assert!(
        case.contexts[3].starts_with("Round 4 of at most 4"),
        "{}",
        case.contexts[3]
    );
    needs_human(&case.result, "b.patch");
    assert!(case.spec.contains("b.patch"), "{}", case.spec);
    assert_eq!(drop_bullets(&case.changes), 0, "{}", case.changes);
}

#[test]
fn without_the_agent_failing_patches_are_kept_and_fuzzy_ones_refreshed() {
    let case = run(
        &[("b.patch", "old"), ("c.patch", "FUZZ")],
        true,
        10,
        Vec::new(),
    );
    assert!(case.contexts.is_empty());
    assert_eq!(case.attempts, 0);
    needs_human(&case.result, "b.patch");
    assert!(case.spec.contains("b.patch"), "{}", case.spec);
    assert_eq!(
        bullets(&case.changes, "Refresh c.patch"),
        1,
        "{}",
        case.changes
    );
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("c.patch")).unwrap(),
        "clean\n"
    );
    assert!(case.notes.contains("--no-agent is set"), "{}", case.notes);
}

#[test]
fn fuzzy_patches_are_refreshed_before_any_turn_and_offset_only_ones_are_left() {
    let case = run(
        &[
            ("b.patch", "old"),
            ("c.patch", "OFFSET"),
            ("d.patch", "FUZZ"),
        ],
        false,
        10,
        vec![Box::new(|_, _| outcome(crate::agent::Action::NoChange, ""))],
    );
    assert_eq!(case.attempts, 1, "{}", case.notes);
    assert_eq!(case.contexts.len(), 1);
    assert!(case.contexts[0].contains("b.patch"), "{}", case.contexts[0]);
    assert!(
        !case.contexts[0].contains("- c.patch: FAILS"),
        "{}",
        case.contexts[0]
    );
    assert!(
        !case.contexts[0].contains("- d.patch: FAILS"),
        "{}",
        case.contexts[0]
    );
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("d.patch")).unwrap(),
        "clean\n"
    );
    assert_eq!(bullets(&case.changes, "Refresh d.patch"), 1);
    // An offset alone is applied by rpm too: left byte-identical, no refresh.
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("c.patch")).unwrap(),
        "OFFSET"
    );
    assert_eq!(bullets(&case.changes, "Refresh c.patch"), 0);
    assert!(
        !case.notes.contains("c.patch"),
        "an offset alone is not a note: {}",
        case.notes
    );
    assert!(
        case.notes.contains("d.patch: refreshed by the pipeline"),
        "{}",
        case.notes
    );
}

#[test]
fn a_failed_turn_keeps_the_edit_quilt_proves_and_rolls_back_the_rest() {
    let case = run_turns(
        &[("a.patch", "old"), ("b.patch", "old")],
        false,
        10,
        &mut |ctx, _| {
            std::fs::write(ctx.checkout_dir.join("a.patch"), "clean\n").unwrap();
            std::fs::write(ctx.checkout_dir.join("b.patch"), "half done").unwrap();
            Err((
                "max turns reached (41 of 40)".to_string(),
                FailureKind::AgentFailed,
            ))
        },
    );
    let Err(PipelineResult::Failed { reason, .. }) = &case.result else {
        panic!("{:?}", case.result);
    };
    assert!(reason.starts_with("the agent turn failed"), "{reason}");
    assert!(
        reason.contains("needs a human") && reason.contains("b.patch"),
        "{reason}"
    );
    assert!(reason.contains("+new"), "{reason}");
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("a.patch")).unwrap(),
        "clean\n"
    );
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("b.patch")).unwrap(),
        "old"
    );
    assert!(case.spec.contains("Patch1: b.patch"), "{}", case.spec);
    assert_eq!(
        bullets(&case.changes, "Refresh a.patch"),
        1,
        "{}",
        case.changes
    );
    assert!(case.notes.contains("- a.patch: kept"), "{}", case.notes);
    assert!(
        case.notes
            .contains("- b.patch: rolled back, its edit does not apply"),
        "{}",
        case.notes
    );
}

#[test]
fn a_per_patch_answer_keeps_the_rebased_patch_and_rolls_back_the_blocked_one() {
    let case = run(
        &[("a.patch", "old"), ("b.patch", "old")],
        false,
        10,
        vec![
            Box::new(|checkout, _| {
                std::fs::write(checkout.join("a.patch"), "clean\n").unwrap();
                std::fs::write(checkout.join("b.patch"), "half done").unwrap();
                let mut out = outcome(crate::agent::Action::NoChange, "");
                out.patch_answers = vec![
                    crate::agent::PatchAnswer {
                        name: "a.patch".into(),
                        status: crate::agent::PatchAnswerStatus::Rebased,
                        reason: "ported".into(),
                    },
                    crate::agent::PatchAnswer {
                        name: "b.patch".into(),
                        status: crate::agent::PatchAnswerStatus::Blocked,
                        reason: "hunk 1 is gone".into(),
                    },
                ];
                out
            }),
            Box::new(|_, _| {
                let mut out = outcome(crate::agent::Action::NoChange, "");
                out.patch_answers = vec![crate::agent::PatchAnswer {
                    name: "b.patch".into(),
                    status: crate::agent::PatchAnswerStatus::Blocked,
                    reason: "hunk 1 is gone".into(),
                }];
                out
            }),
        ],
    );
    needs_human(&case.result, "b.patch");
    match &case.result {
        Err(PipelineResult::Failed { reason, .. }) => assert!(
            reason.contains("b.patch") && reason.contains("blocked by the agent: hunk 1 is gone"),
            "{reason}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("a.patch")).unwrap(),
        "clean\n"
    );
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("b.patch")).unwrap(),
        "old"
    );
    assert_eq!(
        bullets(&case.changes, "Refresh a.patch"),
        1,
        "{}",
        case.changes
    );
    assert!(
        case.notes.contains("b.patch Blocked: hunk 1 is gone"),
        "{}",
        case.notes
    );
    assert!(
        case.notes.contains("a.patch Rebased: ported"),
        "{}",
        case.notes
    );
}

#[test]
fn a_later_answer_replaces_an_earlier_block() {
    use crate::agent::{PatchAnswer, PatchAnswerStatus};
    let answer = |name: &str, status| PatchAnswer {
        name: name.into(),
        status,
        reason: format!("{name} is {status:?}"),
    };
    let mut blocked = Vec::new();
    record_patch_answers(
        &mut blocked,
        &[
            answer("a.patch", PatchAnswerStatus::Blocked),
            answer("b.patch", PatchAnswerStatus::Blocked),
        ],
    );
    record_patch_answers(
        &mut blocked,
        &[answer("a.patch", PatchAnswerStatus::Rebased)],
    );
    let names: Vec<&str> = blocked.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["b.patch"]);
}

#[test]
fn no_turn_runs_past_the_attempt_limit() {
    let case = run(&[("b.patch", "old")], false, 0, Vec::new());
    assert!(case.contexts.is_empty());
    needs_human(&case.result, "b.patch");
    assert!(case.notes.contains("no agent turn left"), "{}", case.notes);
}

#[test]
fn clean_patches_need_no_turn() {
    let case = run(&[("a.patch", "clean")], false, 10, Vec::new());
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(case.contexts.is_empty());
    assert_eq!(case.attempts, 0);
    assert_eq!(bullets(&case.changes, "Refresh a.patch"), 0);
}

#[test]
fn the_generic_gate_names_only_the_unproven_removals() {
    if quilt_missing() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    source_tarball(checkout);
    std::fs::write(checkout.join("up.patch"), UPSTREAM_DIFF).unwrap();
    std::fs::write(checkout.join("a.patch"), FOREIGN_DIFF).unwrap();
    let spec_path = checkout.join("foo.spec");
    let before =
        "Name: foo\nVersion: 1.0\nPatch0: up.patch\nPatch1: a.patch\n%prep\n%autosetup -p1\n";
    std::fs::write(&spec_path, before).unwrap();
    let snapshot = CheckoutSnapshot::take(checkout);
    let mut config = super::hygiene_tests::config("foo", checkout);
    config.version = "1.0".into();
    // Both removed: the files deleted, the lines gone.
    std::fs::remove_file(checkout.join("up.patch")).unwrap();
    std::fs::remove_file(checkout.join("a.patch")).unwrap();
    std::fs::write(
        &spec_path,
        "Name: foo\nVersion: 1.0\n%prep\n%autosetup -p1\n",
    )
    .unwrap();
    let unproven = unproven_patch_removals(&config, checkout, &spec_path, before, Some(&snapshot));
    assert_eq!(unproven, ["a.patch"]);
    assert!(unproven_removal_problem(&unproven).contains("rebase it with quilt"));
    // Nothing removed: nothing to prove.
    std::fs::write(&spec_path, before).unwrap();
    std::fs::write(checkout.join("up.patch"), UPSTREAM_DIFF).unwrap();
    std::fs::write(checkout.join("a.patch"), FOREIGN_DIFF).unwrap();
    assert!(
        unproven_patch_removals(&config, checkout, &spec_path, before, Some(&snapshot)).is_empty()
    );
}

#[test]
fn dropping_a_patch_removes_exactly_its_lines() {
    let spec = "Name: foo\nPatch1: a.patch\nPatch2: b.patch\n%prep\n%setup\n%patch -P 1 -p1\n%patch2 -p1\n";
    let out = without_patch_applications(spec, &["1".to_string()]);
    assert!(!out.contains("-P 1") && out.contains("%patch2"), "{out}");
    let dir = tempfile::tempdir().unwrap();
    let series = "# keep me\na.patch -p0\nb.patch\n  \nc.patch";
    std::fs::write(dir.path().join("series"), series).unwrap();
    drop_series_lines(dir.path(), &["a.patch".to_string()]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("series")).unwrap(),
        "# keep me\nb.patch\n  \nc.patch"
    );
}

#[test]
fn the_refresh_series_is_the_spec_beside_an_untouched_checkout_series() {
    let dir = tempfile::tempdir().unwrap();
    let series = "a.patch -p0\n";
    std::fs::write(dir.path().join("series"), series).unwrap();
    let spec =
        "Name: foo\nPatch0: z.patch\nPatch1: y.patch\n%prep\n%autosetup -p1\n%patch -P 1 -p0\n";
    let (guard, notes) = write_rebase_series(spec, dir.path());
    let scratch = dir.path().join(crate::agent::REFRESH_SERIES);
    assert_eq!(
        std::fs::read_to_string(&scratch).unwrap(),
        "z.patch\ny.patch -p0\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("series")).unwrap(),
        series
    );
    assert!(notes.iter().any(|n| n.contains("a.patch")), "{notes:?}");
    drop(guard);
    assert!(!scratch.exists());
}

#[test]
fn a_bare_patch_line_is_written_to_the_refresh_series_at_level_zero() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: foo\nPatch1: y.patch\n%prep\n%setup -q\n%patch -P 1\n";
    let (guard, _notes) = write_rebase_series(spec, dir.path());
    let scratch = dir.path().join(crate::agent::REFRESH_SERIES);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), "y.patch -p0\n");
    drop(guard);
}

#[test]
fn a_rollback_puts_the_refresh_series_back_without_a_warning() {
    struct Collect(std::sync::Mutex<Vec<String>>);
    impl Reporter for Collect {
        fn report(&self, e: &Event) {
            if let Event::Warning { message, .. } = e {
                self.0.lock().unwrap().push(message.clone());
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    let spec = "Name: foo\nPatch1: y.patch\n%prep\n%setup -q\n%patch -P 1\n";
    let (guard, _) = write_rebase_series(spec, checkout);
    let scratch = checkout.join(crate::agent::REFRESH_SERIES);
    let written = std::fs::read_to_string(&scratch).unwrap();
    // Taken after the pipeline wrote the series, as a round does.
    let snapshot = CheckoutSnapshot::take(checkout);
    let reporter = Collect(Default::default());
    // A turn that removes the series, then one that rewrites it.
    std::fs::remove_file(&scratch).unwrap();
    snapshot.restore_keeping(&reporter, None, &[]);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), written);
    std::fs::write(&scratch, "other.patch\n").unwrap();
    snapshot.restore_keeping(&reporter, None, &[]);
    assert_eq!(std::fs::read_to_string(&scratch).unwrap(), written);
    assert!(reporter.0.lock().unwrap().is_empty(), "{:?}", reporter.0);
    drop(guard);
    assert!(!scratch.exists());
}

#[test]
fn a_checkout_series_that_disagrees_with_the_spec_is_reported() {
    let entries = vec![
        crate::patch_check::SeriesEntry {
            number: "1".into(),
            name: "y.patch".into(),
            level: 0,
        },
        crate::patch_check::SeriesEntry {
            number: "0".into(),
            name: "z.patch".into(),
            level: 1,
        },
    ];
    let notes = series_disagreements("a.patch\ny.patch\ngone.patch -p1\n", &entries, |n| {
        n != "gone.patch"
    });
    let text = notes.join("\n");
    assert!(
        text.contains("names a.patch, which the spec does not apply"),
        "{text}"
    );
    assert!(
        text.contains("applies y.patch with -p1, the spec with -p0"),
        "{text}"
    );
    assert!(
        text.contains("names gone.patch, which the checkout does not have"),
        "{text}"
    );
    assert!(text.contains("does not list z.patch"), "{text}");
    assert!(
        series_disagreements("y.patch -p0\nz.patch\n", &entries, |_| true).is_empty(),
        "an agreeing series has no notes"
    );
}

#[test]
fn an_all_blocked_round_that_changes_nothing_ends_the_loop_at_once() {
    let case = run(
        &[("b.patch", "old")],
        false,
        10,
        // One script only: a second turn would fail the test in `run`.
        vec![Box::new(|_, _| {
            let mut out = outcome(crate::agent::Action::NoChange, "");
            out.patch_answers = vec![crate::agent::PatchAnswer {
                name: "b.patch".into(),
                status: crate::agent::PatchAnswerStatus::Blocked,
                reason: "hunk 1 is gone".into(),
            }];
            out
        })],
    );
    assert_eq!(case.contexts.len(), 1, "{:?}", case.contexts);
    needs_human(&case.result, "b.patch");
    match &case.result {
        Err(PipelineResult::Failed { reason, .. }) => assert!(
            reason.contains("blocked by the agent: hunk 1 is gone")
                && reason.contains("blocked every remaining patch and changed no file"),
            "{reason}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_stall_needs_every_failing_patch_blocked_and_no_file_changed() {
    use crate::agent::{PatchAnswer, PatchAnswerStatus};
    let answer = |name: &str, status| PatchAnswer {
        name: name.into(),
        status,
        reason: String::new(),
    };
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let blocked = PatchAnswerStatus::Blocked;
    let both = [answer("a.patch", blocked), answer("b.patch", blocked)];
    let stall = crate::pipeline::patch_loop::all_blocked_stall;
    assert!(stall(&names(&["a.patch", "b.patch"]), &both, true));
    // A file changed: the round made a difference, so it is not a stall.
    assert!(!stall(&names(&["a.patch", "b.patch"]), &both, false));
    // A failing patch with no blocked answer.
    let one = [answer("a.patch", blocked)];
    assert!(!stall(&names(&["a.patch", "b.patch"]), &one, true));
    let rebased = [
        answer("a.patch", PatchAnswerStatus::Rebased),
        answer("b.patch", blocked),
    ];
    assert!(!stall(&names(&["a.patch", "b.patch"]), &rebased, true));
    // Nothing still failing is not a stall: the check decides.
    assert!(!stall(&[], &both, true));
}

#[test]
fn two_refreshed_patches_share_one_grouped_bullet() {
    let case = run(
        &[("b.patch", "old"), ("c.patch", "FUZZ"), ("d.patch", "FUZZ")],
        false,
        10,
        vec![Box::new(|_, _| outcome(crate::agent::Action::NoChange, ""))],
    );
    assert_eq!(
        bullets(&case.changes, "Refresh patches:"),
        1,
        "{}",
        case.changes
    );
    assert!(
        case.changes.contains("  * c.patch\n  * d.patch\n"),
        "{}",
        case.changes
    );
    assert_eq!(
        bullets(&case.changes, "Refresh c.patch"),
        0,
        "{}",
        case.changes
    );
}
