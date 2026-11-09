//! The gate on a patch an edit keeps declared but stops applying: judged like a
//! removal, by the same proof.

use super::patch_stage_tests::{FOREIGN_DIFF, UPSTREAM_DIFF, quilt_missing, source_tarball};
use super::removal_proof::*;
use crate::patch_check::{Disabled, DropProof};
use std::path::{Path, PathBuf};

/// A patch that applies to the sources of `source_tarball`.
const APPLYING_DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n B\n-c\n+C\n d\n";

fn spec(patch_lines: &str, prep: &str) -> String {
    format!(
        "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: x\nLicense: MIT\n{patch_lines}\
         %description\nx\n\n%prep\n{prep}\n"
    )
}

/// A checkout holding `name` with `content`, and the spec before an edit.
/// Returns the checkout, the spec path (with the before-spec written) and it.
fn checkout(name: &str, content: &str, prep: &str) -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let (spec_path, before) = checkout_in(dir.path(), name, content, prep);
    (dir, spec_path, before)
}

/// [`checkout`] in the existing directory `dir`.
fn checkout_in(dir: &Path, name: &str, content: &str, prep: &str) -> (PathBuf, String) {
    std::fs::write(dir.join(name), content).unwrap();
    let before = spec(&format!("Patch0: {name}\n"), prep);
    let spec_path = dir.join("foo.spec");
    std::fs::write(&spec_path, &before).unwrap();
    (spec_path, before)
}

/// Judge the spec `after` against `before`.
fn judge(dir: &Path, spec_path: &Path, before: &str, after: &str) -> RemovalJudgement {
    std::fs::write(spec_path, after).unwrap();
    judge_removals("1.0", dir, spec_path, before, None)
}

#[test]
fn a_disabled_patch_quilt_did_not_check_is_refused() {
    let (dir, spec_path, before) = checkout("p.patch", "--- a/f\n+++ b/f\n", "%autosetup -p1");
    let after = spec("Patch0: p.patch\n", "%autosetup -p1 -N");
    let judged = judge(dir.path(), &spec_path, &before, &after);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    let refusal = &judged.refused[0];
    assert_eq!(refusal.name, "p.patch");
    assert_eq!(refusal.kind, RemovalKind::Disabled(Disabled::NotApplied));
    assert!(refusal.reason.contains("did not check"), "{refusal:?}");
    let text = unproven_removal_problem(&judged.refused);
    assert!(text.contains("disabling p.patch is refused"), "{text}");
    assert!(text.contains("a human must decide"), "{text}");
    assert!(text.contains("no line of %prep applies it"), "{text}");
}

#[test]
fn a_disabled_security_patch_is_named() {
    let (dir, spec_path, before) =
        checkout("CVE-2024-1.patch", "--- a/f\n+++ b/f\n", "%autosetup -p1");
    let after = spec("Patch0: CVE-2024-1.patch\n", "%autosetup -p1 -N");
    let judged = judge(dir.path(), &spec_path, &before, &after);
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    assert!(judged.refused[0].security);
    let text = unproven_removal_problem(&judged.refused);
    assert!(text.contains("security patch"), "{text}");
}

#[test]
fn the_text_names_the_guard() {
    let (dir, spec_path, before) = checkout("p.patch", "x", "%setup -q\n%patch -P 0 -p1");
    let after = spec(
        "Patch0: p.patch\n",
        "%setup -q\n%if %{with x}\n%patch -P 0 -p1\n%endif",
    );
    let judged = judge(dir.path(), &spec_path, &before, &after);
    let text = unproven_removal_problem(&judged.refused);
    assert!(
        text.contains("only when `%if %{with x} [then]` holds"),
        "{text}"
    );
    let after = spec(
        "%if 0\nPatch0: p.patch\n%endif\n",
        "%setup -q\n%patch -P 0 -p1",
    );
    let judged = judge(dir.path(), &spec_path, &before, &after);
    let text = unproven_removal_problem(&judged.refused);
    assert!(
        text.contains("under `%if 0 [then]`, which is always false"),
        "{text}"
    );
}

#[test]
fn a_disabled_patch_that_applies_is_refused() {
    if quilt_missing() {
        return;
    }
    let (dir, spec_path, before) = checkout("p.patch", APPLYING_DIFF, "%autosetup -p1");
    source_tarball(dir.path());
    let after = spec("Patch0: p.patch\n", "%autosetup -p1 -N");
    let judged = judge(dir.path(), &spec_path, &before, &after);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert!(
        judged.refused[0]
            .reason
            .contains("applies to the new sources"),
        "{judged:?}"
    );
}

#[test]
fn a_disabled_foreign_patch_is_refused() {
    if quilt_missing() {
        return;
    }
    let (dir, spec_path, before) = checkout("p.patch", FOREIGN_DIFF, "%autosetup -p1");
    source_tarball(dir.path());
    let after = spec("Patch0: p.patch\n", "%autosetup -p1 -N");
    let judged = judge(dir.path(), &spec_path, &before, &after);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert!(
        judged.refused[0]
            .reason
            .contains("its hunks are not all in the new tarball"),
        "{judged:?}"
    );
}

#[test]
fn a_disabled_patch_quilt_proves_upstream_is_kept() {
    if quilt_missing() {
        return;
    }
    let before_prep = "%autosetup -p1 -N\n%patch -P 0 -p1";
    let (dir, spec_path, before) = checkout("p.patch", UPSTREAM_DIFF, before_prep);
    source_tarball(dir.path());
    let after = spec(
        "Patch0: p.patch\n",
        "%autosetup -p1 -N\n%if 0\n%patch -P 0 -p1\n%endif",
    );
    let judged = judge(dir.path(), &spec_path, &before, &after);
    assert_eq!(
        judged.proven,
        [("p.patch".to_string(), DropProof::ReverseApplies)]
    );
    assert!(judged.refused.is_empty(), "{judged:?}");
}

#[test]
fn a_proven_disable_by_narrowing_is_still_refused() {
    if quilt_missing() {
        return;
    }
    for (before_prep, narrowed) in [
        ("%autosetup -p1", "%autosetup -p1 -N"),
        ("%autosetup -p1", "%autosetup -p1 -qN"),
        (
            "%autosetup -N\n%autopatch -p1",
            "%autosetup -N\n%autopatch -p1 -m 1",
        ),
    ] {
        let (dir, spec_path, before) = checkout("p.patch", UPSTREAM_DIFF, before_prep);
        source_tarball(dir.path());
        let after = spec("Patch0: p.patch\n", narrowed);
        let judged = judge(dir.path(), &spec_path, &before, &after);
        assert!(judged.proven.is_empty(), "{narrowed}: {judged:?}");
        assert_eq!(judged.refused.len(), 1, "{narrowed}: {judged:?}");
        assert!(judged.refused[0].reason.contains("narrowing"), "{judged:?}");
    }
}

#[test]
fn a_refreshed_patch_under_an_unchanged_spec_is_not_judged() {
    if quilt_missing() {
        return;
    }
    let (dir, spec_path, before) = checkout("p.patch", FOREIGN_DIFF, "%autosetup -p1");
    source_tarball(dir.path());
    std::fs::write(dir.path().join("p.patch"), APPLYING_DIFF).unwrap();
    let judged = judge(dir.path(), &spec_path, &before, &before);
    assert!(
        judged.proven.is_empty() && judged.refused.is_empty(),
        "{judged:?}"
    );
}

#[test]
fn a_removal_quilt_proves_is_still_proven() {
    if quilt_missing() {
        return;
    }
    let (dir, spec_path, before) = checkout("p.patch", UPSTREAM_DIFF, "%autosetup -p1");
    source_tarball(dir.path());
    let snapshot = crate::pipeline::snapshot::CheckoutSnapshot::take(dir.path());
    std::fs::remove_file(dir.path().join("p.patch")).unwrap();
    std::fs::write(&spec_path, spec("", "%autosetup -p1")).unwrap();
    let judged = judge_removals("1.0", dir.path(), &spec_path, &before, Some(&snapshot));
    assert_eq!(
        judged.proven,
        [("p.patch".to_string(), DropProof::ReverseApplies)]
    );
    assert!(judged.refused.is_empty(), "{judged:?}");
}

#[test]
fn a_commented_declaration_and_a_macro_rename_are_removals() {
    let (dir, spec_path, before) = checkout("p.patch", "x", "%autosetup -p1");
    for after in [
        spec("#Patch0: p.patch\n", "%autosetup -p1"),
        spec(
            "%global pname p.patch\nPatch0: %{pname}\n",
            "%autosetup -p1",
        ),
    ] {
        let judged = judge(dir.path(), &spec_path, &before, &after);
        assert_eq!(judged.refused.len(), 1, "{after}: {judged:?}");
        assert_eq!(judged.refused[0].kind, RemovalKind::Removed);
        let text = unproven_removal_problem(&judged.refused);
        assert!(text.contains("removal of p.patch is refused"), "{text}");
    }
}

#[test]
fn the_backstop_refuses_a_disable_and_the_wheel_switch_path_does_not_ask() {
    let (dir, spec_path, before) = checkout("p.patch", "x", "%autosetup -p1");
    let step = ap_core::run::StepDir {
        path: dir.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let after = spec("%if 0\nPatch0: p.patch\n%endif\n", "%autosetup -p1");
    let backstop = |judge_removals: bool| {
        std::fs::write(&spec_path, &after).unwrap();
        crate::pipeline::patch_rules::reject_patch_rules(
            &spec_path,
            &before,
            None,
            &step,
            &ap_core::sink::NullReporter,
            dir.path().join("notes.md"),
            &[],
            judge_removals,
        )
    };
    assert_eq!(backstop(false), None);
    assert_eq!(std::fs::read_to_string(&spec_path).unwrap(), after);
    let refused = backstop(true).expect("refused");
    assert!(
        refused.contains("disabling p.patch is refused"),
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&spec_path).unwrap(), before);
}

/// The `%prep` of an edit that stops applying patch 0 without narrowing the setup.
const OFF: &str = "%setup -q\n%if 0\n%patch -P 0 -p1\n%endif";

/// A fix turn on a package whose patch is `name` (`content`), that edits the
/// spec with `prep` in place of `%autosetup -p1`. Returns the checkout.
fn fix_turn_disabling(name: &str, content: &str, prep: &str) -> (tempfile::TempDir, PathBuf) {
    fix_turn(name, content, prep, true, "Disable the patch")
}

/// [`fix_turn_disabling`] with the snapshot kept or not and the agent's bullet.
fn fix_turn(
    name: &str,
    content: &str,
    prep: &str,
    with_snapshot: bool,
    changelog: &str,
) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("foo");
    std::fs::create_dir_all(&checkout).unwrap();
    source_tarball(&checkout);
    let (_, before) = checkout_in(&checkout, name, content, "%autosetup -p1");
    let spec_path = checkout.join("foo.spec");
    let sep = "-".repeat(67);
    std::fs::write(
        checkout.join("foo.changes"),
        format!("{sep}\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n"),
    )
    .unwrap();
    let snapshot = crate::pipeline::snapshot::CheckoutSnapshot::take(&checkout);
    std::fs::write(&spec_path, before.replace("%autosetup -p1", prep)).unwrap();
    let state = root.path().join("state");
    let mut config = super::hygiene_tests::config("foo", &state);
    config.rpm_name = "foo".into();
    let step = ap_core::run::StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let mut outcome = super::pipeline_wheel_outcome("x");
    outcome.action = crate::agent::Action::Edited;
    outcome.changelog = changelog.into();
    let effect = super::apply_turn::apply_turn_and_reset_with(
        super::apply_turn::ApplyTurnCtx {
            config: &config,
            checkout_dir: &checkout,
            spec_path: &spec_path,
            step: &step,
            before: &before,
            rpm_name: "foo",
            label: "FixAfterBuild",
            attempt: 1,
            notes_path: root.path().join("notes.md"),
            snapshot: with_snapshot.then_some(&snapshot),
            kept: &[],
            staged_out: None,
            rejected_out: None,
        },
        &outcome,
        &ap_core::sink::NullReporter,
        &mut String::new(),
        &mut false,
        &|_| Err("no lookup".to_string()),
    );
    assert!(effect.is_ok(), "{:?}", effect.err());
    (root, checkout)
}

#[test]
fn a_fix_turn_finishes_a_proven_disable_as_a_removal() {
    if quilt_missing() {
        return;
    }
    let (_root, checkout) = fix_turn_disabling("up.patch", UPSTREAM_DIFF, OFF);
    let spec = std::fs::read_to_string(checkout.join("foo.spec")).unwrap();
    assert!(!spec.contains("up.patch"), "{spec}");
    assert!(!checkout.join("up.patch").exists());
    let changes = std::fs::read_to_string(checkout.join("foo.changes")).unwrap();
    assert!(
        changes.contains("- Drop up.patch: already applied upstream (reverse-applies cleanly)"),
        "{changes}"
    );
}

#[test]
fn a_fix_turn_that_disables_a_patch_without_proof_is_rolled_back() {
    if quilt_missing() {
        return;
    }
    let (_root, checkout) = fix_turn_disabling("p.patch", FOREIGN_DIFF, OFF);
    let spec = std::fs::read_to_string(checkout.join("foo.spec")).unwrap();
    assert!(
        spec.contains("Patch0: p.patch") && !spec.contains("-N"),
        "{spec}"
    );
    assert!(checkout.join("p.patch").exists());
    let changes = std::fs::read_to_string(checkout.join("foo.changes")).unwrap();
    assert!(!changes.contains("Drop"), "{changes}");
}

#[test]
fn a_fix_turn_without_a_snapshot_does_not_delete_the_patch() {
    if quilt_missing() {
        return;
    }
    let (_root, checkout) = fix_turn("up.patch", UPSTREAM_DIFF, OFF, false, "Disable the patch");
    assert!(checkout.join("up.patch").exists());
    let spec = std::fs::read_to_string(checkout.join("foo.spec")).unwrap();
    assert!(spec.contains("Patch0: up.patch"), "{spec}");
}

#[test]
fn the_agents_own_drop_bullet_is_not_written_twice() {
    if quilt_missing() {
        return;
    }
    let (_root, checkout) = fix_turn(
        "up.patch",
        UPSTREAM_DIFF,
        OFF,
        true,
        "Drop up.patch: upstream merged it",
    );
    let changes = std::fs::read_to_string(checkout.join("foo.changes")).unwrap();
    assert_eq!(changes.matches("Drop up.patch").count(), 1, "{changes}");
}

#[test]
fn a_spec_that_cannot_be_written_keeps_the_patch_file() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, spec_path, before) = checkout("p.patch", "x", "%autosetup -p1");
    let after = spec("%if 0\nPatch0: p.patch\n%endif\n", "%autosetup -p1");
    std::fs::write(&spec_path, &after).unwrap();
    std::fs::set_permissions(&spec_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let judged = RemovalJudgement {
        proven: vec![("p.patch".to_string(), DropProof::ReverseApplies)],
        disabled: vec!["p.patch".to_string()],
        ..Default::default()
    };
    let snapshot = crate::pipeline::snapshot::CheckoutSnapshot::take(dir.path());
    let result = finish_proven_disables(dir.path(), &spec_path, &before, &judged, Some(&snapshot));
    // A root user can write a read-only file; the rest of the assertions need the failure.
    if result.is_err() {
        assert!(dir.path().join("p.patch").exists());
    } else {
        assert!(!dir.path().join("p.patch").exists());
    }
}
