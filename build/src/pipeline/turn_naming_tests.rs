//! The `.changes` name of a patch an agent edit adds, on the paths that keep an
//! agent edit: the Edited arm (fix, review and dependency turns) and the gate of
//! the arms that answer `NoChange` with the edit kept.

use super::*;

const BEFORE: &str =
    "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\n\n%prep\n%autosetup -p1\n";

/// A patch header that passes the rules (Subject, Description, Upstream).
const HEADER: &str = "Subject: Fix the build with gcc 15\n\
Description: gcc 15 rejects the implicit int in foo.c, so the build stops.\n\
Upstream: not yet reported\n\
---\n";

const DIFF: &str = "--- foo-1.0/f.txt\n+++ foo-1.0/f.txt\n@@ -1 +1 @@\n-x\n+y\n";

/// The spec `BEFORE` with a commented `Patch0:` line for `foo-fix.patch`.
fn spec_with_new_patch() -> String {
    format!("{BEFORE}# PATCH-FIX-UPSTREAM foo-fix.patch -- gcc 15 build\nPatch0: foo-fix.patch\n")
}

/// A package checkout with `BEFORE` as its spec, a release entry in `.changes`,
/// and the turn's new patch written by `write_patch` after the snapshot.
struct Case {
    _root: tempfile::TempDir,
    checkout: PathBuf,
    spec_path: PathBuf,
    config: PipelineConfig,
    step: StepDir,
    snapshot: CheckoutSnapshot,
}

fn case() -> Case {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkout = root.path().join("proj").join("foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let config = super::hygiene_tests::config("foo", &state_root);
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, BEFORE).unwrap();
    let sep = "-".repeat(67);
    std::fs::write(
        checkout.join("foo.changes"),
        format!("{sep}\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n"),
    )
    .unwrap();
    let snapshot = CheckoutSnapshot::take(&checkout);
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    Case {
        _root: root,
        checkout,
        spec_path,
        config,
        step,
        snapshot,
    }
}

/// The turn of `case` writes the spec with the new patch and the patch file.
fn agent_adds_patch(c: &Case, patch_text: &str) {
    std::fs::write(c.spec_path.clone(), spec_with_new_patch()).unwrap();
    std::fs::write(c.checkout.join("foo-fix.patch"), patch_text).unwrap();
}

fn changes(c: &Case) -> String {
    std::fs::read_to_string(c.checkout.join("foo.changes")).unwrap()
}

/// An `Edited` answer with `changelog` as its `.changes` bullet.
fn edited(changelog: &str) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = crate::agent::Action::Edited;
    out.reasoning = "scripted".into();
    out.changelog = changelog.into();
    out
}

/// Runs the Edited arm of `label` on `c` and returns its effect and the reason it
/// gave for a rejected edit.
fn apply(
    c: &Case,
    label: &str,
    outcome: &crate::agent::TurnOutcome,
) -> (TurnEffect, Option<String>) {
    let lookup = |_: &PipelineConfig| Err::<String, String>("no PyPI".to_string());
    let notes = c.checkout.join("notes.md");
    let mut rejected = None;
    let (mut sdist, mut from_repo) = (String::new(), false);
    let effect = apply_turn_and_reset_with(
        ApplyTurnCtx {
            config: &c.config,
            checkout_dir: &c.checkout,
            spec_path: &c.spec_path,
            step: &c.step,
            before: BEFORE,
            rpm_name: "foo",
            label,
            attempt: 1,
            notes_path: notes,
            snapshot: Some(&c.snapshot),
            kept: &[],
            staged_out: None,
            rejected_out: Some(&mut rejected),
        },
        outcome,
        &ap_core::sink::NULL_REPORTER,
        &mut sdist,
        &mut from_repo,
        &lookup,
    )
    .expect("the turn is not blocked");
    (effect, rejected)
}

#[test]
fn a_fix_or_review_turn_that_adds_a_patch_must_name_it_in_the_changelog() {
    for label in ["FixAfterBuild — attempt 1", "Review — pre-build"] {
        let c = case();
        agent_adds_patch(&c, &format!("{HEADER}{DIFF}"));
        let (effect, rejected) = apply(&c, label, &edited("Fix the gcc 15 build"));
        assert_eq!(effect, TurnEffect::NoChange, "{label}");
        let why = rejected.expect("the edit is rejected");
        assert!(why.contains("foo-fix.patch"), "{why}");
        assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), BEFORE);
        assert!(!c.checkout.join("foo-fix.patch").exists(), "{label}");
        assert!(!changes(&c).contains("gcc 15 build"), "{}", changes(&c));
    }
}

#[test]
fn a_fix_turn_that_names_the_new_patch_keeps_the_edit_and_writes_its_bullet() {
    let c = case();
    agent_adds_patch(&c, &format!("{HEADER}{DIFF}"));
    let (effect, rejected) = apply(
        &c,
        "FixAfterBuild — attempt 1",
        &edited("Add foo-fix.patch: gcc 15 build"),
    );
    assert_eq!(effect, TurnEffect::Edited, "{rejected:?}");
    assert!(rejected.is_none());
    assert!(
        std::fs::read_to_string(&c.spec_path)
            .unwrap()
            .contains("Patch0: foo-fix.patch")
    );
    assert!(
        changes(&c).contains("Add foo-fix.patch: gcc 15 build"),
        "{}",
        changes(&c)
    );
}

#[test]
fn a_gated_edit_that_adds_a_patch_is_named_before_it_is_kept() {
    // The arms that answer NoChange keep their spec edit through this gate.
    let gate = |c: &Case, bullet: &str| {
        gate_turn_edit_without_accept(
            &c.spec_path,
            BEFORE,
            Some(&c.snapshot),
            &c.step,
            &ap_core::sink::NULL_REPORTER,
            "\n## test — edits kept\n",
            c.checkout.join("notes.md"),
            &[],
            &c.config,
            &c.checkout,
            "PatchDependency - test",
            bullet,
        )
    };
    let c = case();
    agent_adds_patch(&c, &format!("{HEADER}{DIFF}"));
    assert!(
        !gate(&c, "Fix the gcc 15 build"),
        "an unnamed patch is refused"
    );
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), BEFORE);
    assert!(!c.checkout.join("foo-fix.patch").exists());

    let c = case();
    agent_adds_patch(&c, &format!("{HEADER}{DIFF}"));
    assert!(gate(&c, "Add foo-fix.patch: gcc 15 build"));
    assert!(
        std::fs::read_to_string(&c.spec_path)
            .unwrap()
            .contains("Patch0: foo-fix.patch")
    );
    assert!(
        changes(&c).contains("Add foo-fix.patch: gcc 15 build"),
        "{}",
        changes(&c)
    );
}

#[test]
fn an_edit_with_no_changelog_writes_no_bullet_and_not_its_reasoning() {
    let c = case();
    let after = format!("{BEFORE}BuildRequires:  cmake\n");
    std::fs::write(&c.spec_path, &after).unwrap();
    let mut out = edited("");
    out.reasoning = "I added the missing cmake dependency".into();
    let (effect, rejected) = apply(&c, "FixAfterBuild — attempt 1", &out);
    assert_eq!(effect, TurnEffect::Edited, "{rejected:?}");
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), after);
    let text = changes(&c);
    assert!(!text.contains("cmake"), "{text}");
    assert!(!text.contains("Update the packaging"), "{text}");
}

#[test]
fn an_edit_that_changes_more_than_dependencies_needs_a_changelog() {
    let c = case();
    let after = format!("{BEFORE}%files\n%{{_bindir}}/foo\n");
    std::fs::write(&c.spec_path, &after).unwrap();
    let (effect, rejected) = apply(&c, "FixAfterBuild — attempt 1", &edited(""));
    assert_eq!(effect, TurnEffect::NoChange);
    let why = rejected.expect("the edit is rejected");
    assert!(
        why.contains("changelog") && why.contains("convention 40"),
        "{why}"
    );
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), BEFORE);

    // With its bullet the same edit is kept and the bullet is written.
    let c = case();
    std::fs::write(&c.spec_path, &after).unwrap();
    let (effect, rejected) = apply(
        &c,
        "FixAfterBuild — attempt 1",
        &edited("Own the foo binary"),
    );
    assert_eq!(effect, TurnEffect::Edited, "{rejected:?}");
    assert!(
        changes(&c).contains("Own the foo binary"),
        "{}",
        changes(&c)
    );
}

#[test]
fn a_conditional_around_a_check_line_needs_a_changelog() {
    let before = format!("{BEFORE}%check\npytest\n");
    let c = case();
    std::fs::write(&c.spec_path, &before).unwrap();
    let snapshot = CheckoutSnapshot::take(&c.checkout);
    let after = before.replace("pytest\n", "%if 0\npytest\n%endif\n");
    std::fs::write(&c.spec_path, &after).unwrap();
    let why = reject_missing_changelog(
        &c.spec_path,
        &before,
        Some(&snapshot),
        &c.step,
        &ap_core::sink::NULL_REPORTER,
        c.checkout.join("notes.md"),
        &[],
        "",
        false,
        "FixAfterBuild",
    )
    .expect("the edit is rejected");
    assert!(why.contains("changelog"), "{why}");
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), before);
}

#[test]
fn an_rpmlintrc_only_edit_needs_a_changelog_on_the_gate_path_too() {
    let c = case();
    std::fs::write(
        c.checkout.join("foo-rpmlintrc"),
        "# reason: vendor binary\naddFilter(\"missing-hash-section\")\n",
    )
    .unwrap();
    let after = format!("{BEFORE}Source99:       foo-rpmlintrc\n");
    std::fs::write(&c.spec_path, &after).unwrap();
    let kept = gate_turn_edit_without_accept(
        &c.spec_path,
        BEFORE,
        Some(&c.snapshot),
        &c.step,
        &ap_core::sink::NULL_REPORTER,
        "\n## test — edits kept\n",
        c.checkout.join("notes.md"),
        &[],
        &c.config,
        &c.checkout,
        "PatchDependency - test",
        "",
    );
    assert!(!kept, "an rpmlintrc and a Source line need a bullet");
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), BEFORE);
    assert!(!c.checkout.join("foo-rpmlintrc").exists());
}

#[test]
fn an_edit_that_writes_an_rpmlintrc_without_reasons_is_rolled_back() {
    let c = case();
    let after = format!("{BEFORE}Source99:       foo-rpmlintrc\n");
    std::fs::write(&c.spec_path, &after).unwrap();
    std::fs::write(c.checkout.join("foo-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let (effect, rejected) = apply(&c, "FixAfterBuild — attempt 1", &edited("Add an rpmlintrc"));
    assert_eq!(effect, TurnEffect::NoChange);
    let why = rejected.expect("the edit is rejected");
    assert!(
        why.contains("foo-rpmlintrc") && why.contains("# reason:"),
        "{why}"
    );
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), BEFORE);
    assert!(!c.checkout.join("foo-rpmlintrc").exists());

    let c = case();
    std::fs::write(&c.spec_path, &after).unwrap();
    std::fs::write(
        c.checkout.join("foo-rpmlintrc"),
        "# reason: vendor binary\naddFilter(\"missing-hash-section\")\n",
    )
    .unwrap();
    let (effect, rejected) = apply(&c, "FixAfterBuild — attempt 1", &edited("Add an rpmlintrc"));
    assert_eq!(effect, TurnEffect::Edited, "{rejected:?}");
    assert!(c.checkout.join("foo-rpmlintrc").exists());
}
