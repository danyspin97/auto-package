use super::*;
use crate::patch_check::{CheckReport, PatchResult, PatchStatus, Refresh};

mod new_patches;
mod part2;

/// A scripted turn: what it does to the checkout, then its answer.
type Script = Box<dyn FnMut(&Path, &Path) -> crate::agent::TurnOutcome>;

fn outcome(action: crate::agent::Action, changelog: &str) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.reasoning = "scripted".into();
    out.changelog = changelog.into();
    out
}

fn quilt_missing() -> bool {
    let missing = std::process::Command::new("quilt")
        .arg("--version")
        .env("QUILTRC", "/dev/null")
        .output()
        .map_or(true, |o| !o.status.success());
    if missing {
        eprintln!("quilt is not installed; skipping");
    }
    missing
}

/// The check a test runs instead of quilt: a patch file holding `clean`
/// applies, one holding `FUZZ` applies with fuzz and one holding `OFFSET` with
/// an offset (both refresh to `clean` in `Refresh::Offset`), one holding
/// `UPSTREAM` is already applied, anything else fails.
fn fake_check(ctx: &RunCtx, refresh: Refresh) -> CheckReport {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap();
    let patches = crate::patch_check::declared_patches(&spec)
        .into_iter()
        .map(|(_, name)| {
            let body = std::fs::read_to_string(ctx.checkout_dir.join(&name)).unwrap_or_default();
            let (status, refreshed) = if body.contains("FUZZ") {
                (
                    PatchStatus::AppliesWithOffsetOrFuzz {
                        fuzz: true,
                        output: "Hunk #1 succeeded at 3 with fuzz 1.".into(),
                    },
                    (refresh == Refresh::Offset).then(|| b"clean\n".to_vec()),
                )
            } else if body.contains("OFFSET") {
                (
                    PatchStatus::AppliesWithOffsetOrFuzz {
                        fuzz: false,
                        output: "Hunk #1 succeeded at 5 (offset 2 lines).".into(),
                    },
                    (refresh == Refresh::Offset).then(|| b"clean\n".to_vec()),
                )
            } else if body.contains("UPSTREAM") {
                (
                    PatchStatus::AlreadyApplied {
                        output: format!("Patch {name} can be reverse-applied"),
                    },
                    None,
                )
            } else if body.contains("clean") {
                (PatchStatus::Applies, None)
            } else {
                (
                    PatchStatus::Fails {
                        output: format!("Patch {name} does not apply (enforce with -f)"),
                        rejects: "--- f.rej\n-old\n+new\n".into(),
                    },
                    None,
                )
            };
            PatchResult {
                name,
                level: 1,
                status,
                refreshed,
            }
        })
        .collect();
    CheckReport {
        patches,
        not_run: None,
    }
}

struct Case {
    _root: tempfile::TempDir,
    result: Result<(), PipelineResult>,
    contexts: Vec<String>,
    attempts: usize,
    notes: String,
    changes: String,
    spec: String,
    checkout: PathBuf,
}

/// The new sources of the real-quilt cases: `f.txt` has `B` on its second
/// line already.
fn source_tarball(checkout: &Path) {
    let file = std::fs::File::create(checkout.join("foo-1.0.tar.gz")).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    let body = b"a\nB\nc\nd\n";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    ar.append_data(&mut h, "foo-1.0/f.txt", &body[..]).unwrap();
    ar.into_inner().unwrap().finish().unwrap();
}

/// A patch whose change `source_tarball` already has.
const UPSTREAM_DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n";
/// A patch that neither applies nor reverse-applies to `source_tarball`.
const FOREIGN_DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-x\n+X\n c\n d\n";

/// Run `patch_stage_with` on a package whose spec declares `patches` (name,
/// content), with `scripts` as the agent's turns.
fn run(
    patches: &[(&str, &str)],
    no_agent: bool,
    max_attempts: usize,
    scripts: Vec<Script>,
) -> Case {
    let mut queue = std::collections::VecDeque::from(scripts);
    run_turns(patches, no_agent, max_attempts, &mut |ctx, _| {
        let mut script = queue
            .pop_front()
            .expect("the loop asked for more turns than the script has");
        Ok(script(&ctx.checkout_dir, &ctx.spec_path))
    })
}

/// [`run`] with each turn answered by `turn`, which may also fail.
fn run_turns(
    patches: &[(&str, &str)],
    no_agent: bool,
    max_attempts: usize,
    turn: &mut dyn FnMut(&RunCtx, &str) -> Result<crate::agent::TurnOutcome, (String, FailureKind)>,
) -> Case {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.no_agent = no_agent;
    config.budget_usd = 100.0;
    config.max_attempts = max_attempts;
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    source_tarball(&checkout);
    let mut spec = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\n".to_string();
    for (i, (name, body)) in patches.iter().enumerate() {
        spec.push_str(&format!("Patch{i}: {name}\n"));
        std::fs::write(checkout.join(name), body).unwrap();
    }
    spec.push_str("\n%prep\n%autosetup -p1\n");
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, &spec).unwrap();
    // The release entry the bullets fold into (no `osc vc` runs).
    let sep = "-".repeat(67);
    std::fs::write(
        checkout.join("foo.changes"),
        format!("{sep}\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n"),
    )
    .unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
    };
    let step_path = root.path().join("step");
    std::fs::create_dir_all(&step_path).unwrap();
    let mut contexts = Vec::new();
    let result = patch_stage_with(&ctx, &mut st, &mut fake_check, &mut |ctx, _, _, context| {
        contexts.push(context.to_string());
        let out = turn(ctx, context)?;
        Ok((
            StepDir {
                path: step_path.clone(),
                id: ap_core::event::StepId(2),
            },
            out,
        ))
    });
    let notes = std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default();
    Case {
        _root: root,
        result,
        contexts,
        attempts: st.attempts,
        notes,
        changes: std::fs::read_to_string(checkout.join("foo.changes")).unwrap(),
        spec: std::fs::read_to_string(&spec_path).unwrap(),
        checkout,
    }
}

fn bullets(changes: &str, bullet: &str) -> usize {
    changes
        .lines()
        .filter(|l| l.trim() == format!("- {bullet}"))
        .count()
}

fn drop_bullets(changes: &str) -> usize {
    changes
        .lines()
        .filter(|l| l.trim().starts_with("- Drop "))
        .count()
}

fn needs_human(result: &Result<(), PipelineResult>, patch: &str) {
    match result {
        Err(PipelineResult::Failed { reason, .. }) => {
            assert!(
                reason.starts_with("needs a human") && reason.contains(patch),
                "{reason}"
            );
            assert!(reason.contains("+new"), "the reject hunks: {reason}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_turn_that_rebases_the_failing_patch_ends_the_loop_with_its_bullet() {
    let case = run(
        &[("a.patch", "clean"), ("b.patch", "old hunks")],
        false,
        10,
        vec![Box::new(|checkout, _| {
            std::fs::write(checkout.join("b.patch"), "clean, rebased").unwrap();
            outcome(crate::agent::Action::Edited, "Refresh b.patch")
        })],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 1);
    let context = &case.contexts[0];
    assert!(
        context.contains("b.patch: FAILS") && context.contains("+new"),
        "{context}"
    );
    assert!(context.contains("a.patch: applies cleanly"), "{context}");
    assert_eq!(case.attempts, 1, "a patch turn counts as an attempt");
    assert_eq!(
        bullets(&case.changes, "Refresh b.patch"),
        1,
        "{}",
        case.changes
    );
    assert_eq!(
        bullets(&case.changes, "Refresh a.patch"),
        0,
        "{}",
        case.changes
    );
    assert!(case.spec.contains("Patch1: b.patch"), "{}", case.spec);
    assert!(
        case.notes
            .contains("b.patch: refreshed by the agent (round 1)"),
        "{}",
        case.notes
    );
    // The turn's series is the round's scratch file: the checkout has none, and
    // none is left behind.
    assert!(!case.checkout.join("series").exists());
    assert!(!case.checkout.join(crate::agent::REFRESH_SERIES).exists());
}

#[test]
fn a_patch_that_still_fails_is_kept_and_the_package_needs_a_human() {
    let case = run(
        &[("a.patch", "clean"), ("b.patch", "old hunks")],
        false,
        10,
        vec![Box::new(|checkout, _| {
            // A speculative edit the no_change answer must not keep.
            std::fs::write(checkout.join("b.patch"), "half done").unwrap();
            outcome(crate::agent::Action::NoChange, "")
        })],
    );
    assert_eq!(case.contexts.len(), 1, "no progress: no second round");
    needs_human(&case.result, "b.patch");
    assert!(case.spec.contains("Patch1: b.patch"), "{}", case.spec);
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("b.patch")).unwrap(),
        "old hunks"
    );
    assert_eq!(drop_bullets(&case.changes), 0, "{}", case.changes);
    assert!(
        case.notes.contains("b.patch: still does not apply; kept"),
        "{}",
        case.notes
    );
}

#[test]
fn a_patch_quilt_proves_already_applied_is_removed_without_a_turn() {
    let case = run(
        &[("a.patch", "clean"), ("up.patch", "UPSTREAM")],
        false,
        10,
        Vec::new(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(case.contexts.is_empty());
    assert!(!case.spec.contains("up.patch"), "{}", case.spec);
    assert!(!case.checkout.join("up.patch").exists());
    assert_eq!(
        bullets(
            &case.changes,
            "Drop up.patch: already applied upstream (reverse-applies cleanly)"
        ),
        1,
        "{}",
        case.changes
    );
    assert_eq!(drop_bullets(&case.changes), 1, "{}", case.changes);
}

#[test]
fn a_removal_quilt_does_not_prove_is_rolled_back_and_fed_back() {
    if quilt_missing() {
        return;
    }
    let case = run(
        &[("a.patch", FOREIGN_DIFF)],
        false,
        10,
        vec![
            Box::new(|checkout, spec_path| {
                // "It does not apply, so it is obsolete": not a proof.
                let spec = std::fs::read_to_string(spec_path).unwrap();
                std::fs::write(spec_path, spec.replace("Patch0: a.patch\n", "")).unwrap();
                std::fs::remove_file(checkout.join("a.patch")).unwrap();
                outcome(
                    crate::agent::Action::Edited,
                    "Drop a.patch: no longer applies",
                )
            }),
            Box::new(|_, _| outcome(crate::agent::Action::NoChange, "")),
        ],
    );
    assert_eq!(case.contexts.len(), 2, "the refusal is fed back once");
    assert!(
        case.contexts[1].contains("removal of a.patch is not proven"),
        "{}",
        case.contexts[1]
    );
    needs_human(&case.result, "a.patch");
    assert!(case.spec.contains("Patch0: a.patch"), "{}", case.spec);
    assert_eq!(
        std::fs::read_to_string(case.checkout.join("a.patch")).unwrap(),
        FOREIGN_DIFF
    );
    assert_eq!(drop_bullets(&case.changes), 0, "{}", case.changes);
}

#[test]
fn a_removal_quilt_proves_is_kept_with_its_bullet() {
    if quilt_missing() {
        return;
    }
    // The fake check calls `up.patch` failing; the gate's real quilt run on
    // the new sources proves it already applied.
    let case = run(
        &[("up.patch", UPSTREAM_DIFF), ("b.patch", "old")],
        false,
        10,
        vec![Box::new(|checkout, spec_path| {
            let spec = std::fs::read_to_string(spec_path).unwrap();
            std::fs::write(spec_path, spec.replace("Patch0: up.patch\n", "")).unwrap();
            std::fs::write(checkout.join("b.patch"), "clean").unwrap();
            outcome(crate::agent::Action::Edited, "")
        })],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(!case.spec.contains("up.patch"), "{}", case.spec);
    assert!(!case.checkout.join("up.patch").exists());
    assert_eq!(
        bullets(
            &case.changes,
            "Drop up.patch: already applied upstream (reverse-applies cleanly)"
        ),
        1,
        "{}",
        case.changes
    );
    assert_eq!(
        bullets(&case.changes, "Refresh b.patch"),
        1,
        "{}",
        case.changes
    );
    assert_eq!(
        drop_bullets(&case.changes),
        1,
        "one bullet per patch: {}",
        case.changes
    );
}
