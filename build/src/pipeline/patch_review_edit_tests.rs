//! Tests of the review of the patches a fix turn writes, with a stub review turn.

use super::patch_review_edit::*;
use super::*;
use crate::agent::{Action, PatchReviewAnswer, PatchReviewVerdict as V};

const SPEC: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: t\nLicense: MIT\nPatch0: a.patch\n\n%prep\n%autosetup -p1\n";
const A: &str = "Subject: s\nDescription: d\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+one\n";
const NEW: &str = "Subject: n\nDescription: gcc 15 rejects the implicit int, so the build stops.\nUpstream: not yet reported\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+two\n";

fn said(verdict: V) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = Action::NoChange;
    out.patch_review = Some(PatchReviewAnswer {
        verdict,
        reason: "because".into(),
        problems: Vec::new(),
    });
    out
}

struct Done {
    effect: Result<TurnEffect, PipelineResult>,
    reviews: usize,
    spec: String,
    has_new: bool,
    notes: String,
    record: bool,
}

/// A fix turn that does `edit` to the checkout, then `effect`, reviewed with `verdict`.
fn edited(edit: &dyn Fn(&Path), effect: TurnEffect, verdict: V) -> Done {
    edited_with(&|_| {}, edit, effect, verdict)
}

/// [`edited`] with `prepare` run on the checkout before the turn is recorded.
fn edited_with(
    prepare: &dyn Fn(&Path),
    edit: &dyn Fn(&Path),
    effect: TurnEffect,
    verdict: V,
) -> Done {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.budget_usd = 10.0;
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, SPEC).unwrap();
    std::fs::write(checkout.join("a.patch"), A).unwrap();
    let run_dir = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run_dir.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 1,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
    };
    let step_path = root.path().join("step");
    std::fs::create_dir_all(&step_path).unwrap();
    let step = StepDir {
        path: step_path,
        id: ap_core::event::StepId(2),
    };
    prepare(&checkout);
    begin_turn(&ctx.ws, &spec_path).unwrap();
    let snapshot = CheckoutSnapshot::take(&checkout);
    edit(&checkout);
    let mut reviews = 0;
    let effect = review_turn_patches_with(
        &ctx,
        &mut st,
        &step,
        &snapshot,
        SPEC,
        "FixAfterBuild",
        effect,
        &mut |_, _| Ok(()),
        &mut |_, _, _, _| {
            reviews += 1;
            Ok((
                StepDir {
                    path: root.path().join("step"),
                    id: ap_core::event::StepId(3),
                },
                said(verdict),
            ))
        },
    );
    Done {
        effect,
        reviews,
        spec: std::fs::read_to_string(&spec_path).unwrap(),
        has_new: checkout.join("new.patch").exists(),
        notes: std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default(),
        record: ctx.ws.dir().join("patch-review/in-progress").exists(),
    }
}

/// The turn adds `new.patch` and declares it.
fn add_patch(checkout: &Path) {
    std::fs::write(checkout.join("new.patch"), NEW).unwrap();
    let spec = SPEC.replacen("\n%prep", "Patch1: new.patch\n\n%prep", 1);
    let spec = spec.replacen(
        "Patch0: a.patch\nPatch1",
        "# PATCH-FIX-OPENSUSE a.patch\nPatch0: a.patch\n# PATCH-FIX-UPSTREAM new.patch\nPatch1",
        1,
    );
    std::fs::write(checkout.join("foo.spec"), spec).unwrap();
}

#[test]
fn an_edited_turn_that_adds_a_patch_is_reviewed_and_kept_on_accept() {
    let done = edited(&add_patch, TurnEffect::Edited, V::Accept);
    assert_eq!(done.effect.unwrap(), TurnEffect::Edited);
    assert_eq!(done.reviews, 1);
    assert!(done.has_new);
    assert!(done.spec.contains("Patch1: new.patch"), "{}", done.spec);
}

#[test]
fn a_reject_rolls_the_whole_turn_back_and_notes_the_reason() {
    let done = edited(&add_patch, TurnEffect::Edited, V::Reject);
    assert_eq!(done.effect.unwrap(), TurnEffect::NoChange);
    assert!(!done.has_new);
    assert_eq!(done.spec, SPEC);
    assert!(
        done.notes.contains("rolled back by the patch review"),
        "{}",
        done.notes
    );
}

#[test]
fn a_spec_only_edit_starts_no_review() {
    let done = edited(
        &|co| {
            std::fs::write(
                co.join("foo.spec"),
                SPEC.replace("Summary: t", "Summary: u"),
            )
            .unwrap()
        },
        TurnEffect::Edited,
        V::Reject,
    );
    assert_eq!(done.effect.unwrap(), TurnEffect::Edited);
    assert_eq!(done.reviews, 0);
}

#[test]
fn a_header_only_change_to_a_patch_starts_no_review() {
    let done = edited(
        &|co| {
            std::fs::write(
                co.join("a.patch"),
                A.replace("Subject: s", "Subject: a better one"),
            )
            .unwrap()
        },
        TurnEffect::Edited,
        V::Reject,
    );
    assert_eq!(done.effect.unwrap(), TurnEffect::Edited);
    assert_eq!(done.reviews, 0);
}

#[test]
fn a_changed_line_in_a_declared_patch_is_reviewed_and_a_no_change_is_not() {
    let changed = edited(
        &|co| std::fs::write(co.join("a.patch"), A.replace("+one", "+three")).unwrap(),
        TurnEffect::Edited,
        V::Accept,
    );
    assert_eq!(changed.reviews, 1);
    let none = edited(&add_patch, TurnEffect::NoChange, V::Reject);
    assert_eq!(none.reviews, 0);
    assert_eq!(none.effect.unwrap(), TurnEffect::NoChange);
}

/// The context the real apply check runs in, with the spec `spec` on disk.
fn check_with(spec: &str, tarball: bool) -> Result<(), String> {
    check_named(spec, tarball, "a.patch")
}

fn check_named(spec: &str, tarball: bool, name: &str) -> Result<(), String> {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("foo.spec"), spec).unwrap();
    std::fs::write(checkout.join("a.patch"), A).unwrap();
    if tarball {
        let file = std::fs::File::create(checkout.join("foo-1.0.tar.gz")).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        let body = b"x\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "foo-1.0/f.txt", &body[..]).unwrap();
        ar.into_inner().unwrap().finish().unwrap();
    }
    let run_dir = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run_dir.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: checkout.join("foo.spec"),
        signals: Default::default(),
    };
    applies_to_pristine(&ctx, name)
}

#[test]
fn the_apply_gate_fails_closed() {
    // No source archive, a patch with another level: nothing proves it applies.
    let err = check_with(SPEC, false).unwrap_err();
    assert!(err.contains("no source archive"), "{err}");
    // A %setup spec, and a name with a directory: the check does not run on them.
    let setup = SPEC.replace("%autosetup -p1", "%setup -q\n%patch0 -p1");
    assert!(
        check_with(&setup, true)
            .unwrap_err()
            .contains("no %autosetup")
    );
    let dir_name = SPEC.replace("Patch0: a.patch", "Patch0: sub/a.patch");
    assert!(
        check_named(&dir_name, true, "sub/a.patch")
            .unwrap_err()
            .contains("bare file name")
    );
    let p0 = SPEC.replace("-p1", "-p0");
    assert!(check_with(&p0, true).unwrap_err().contains("-p0"));
    // With the archive the real check runs: `patch` is needed for the verdict.
    if std::process::Command::new("patch")
        .arg("--version")
        .output()
        .is_ok()
    {
        // f.txt holds `x`, so the patch's `-x +one` applies.
        assert_eq!(check_with(SPEC, true), Ok(()));
        let broken = SPEC.replace("Patch0: a.patch", "Patch0: a.patch\nPatch1: missing.patch");
        // A patch not on disk is not the reviewed one: it does not decide.
        assert_eq!(check_with(&broken, true), Ok(()));
    }
}

#[test]
fn the_turn_record_goes_with_a_settled_turn_and_stays_for_one_that_cannot_be_put_back() {
    // Accepted, rolled back, no change: settled, no record.
    assert!(!edited(&add_patch, TurnEffect::Edited, V::Accept).record);
    assert!(!edited(&add_patch, TurnEffect::Edited, V::Reject).record);
    assert!(!edited(&|_| {}, TurnEffect::NoChange, V::Reject).record);
    // A patch over the snapshot's cap has no kept contents: a rejected rewrite of
    // it cannot be put back, so the record stays and the turn stops.
    let big = format!("{A}{}", "# padding line\n".repeat(1_200_000));
    let done = edited_with(
        &|co| std::fs::write(co.join("a.patch"), &big).unwrap(),
        &|co| std::fs::write(co.join("a.patch"), big.replace("+one", "+three")).unwrap(),
        TurnEffect::Edited,
        V::Reject,
    );
    match done.effect {
        Err(PipelineResult::Failed { kind, reason, .. }) => {
            assert_eq!(kind, FailureKind::NeedsMaintainer);
            assert!(reason.contains("could not be rolled back"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(done.record);
}
