//! The turn rounds of the bug fix stage.

use super::*;
use crate::agent::{Action, FixAnswer, FixVerdict};
use crate::pipeline::bug_fix_rounds::*;

const COMMENT: &str = "# PATCH-FIX-OPENSUSE foo-CVE-2025-1234.patch bsc#1234567 CVE-2025-1234 -- check the length, needs submitting upstream";
/// A diff that does not apply to the sources of `source_tarball`.
const BAD_DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n x\n-c\n+C\n d\n";

type Reply = Result<crate::agent::TurnOutcome, (String, FailureKind)>;

fn answer(
    action: Action,
    verdict: FixVerdict,
    reason: &str,
    evidence: &[&str],
) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.reasoning = "scripted".into();
    out.changelog = format!("Add {NAME}: check the length");
    out.bug_fix = Some(FixAnswer {
        verdict,
        patch: NAME.into(),
        upstream: String::new(),
        reason: reason.into(),
        evidence: evidence.iter().map(|e| e.to_string()).collect(),
    });
    out
}

fn fixed() -> crate::agent::TurnOutcome {
    answer(
        Action::Edited,
        FixVerdict::Fixed,
        "the length is checked",
        &[],
    )
}

/// The patch the turn writes, with no upstream (`written`), and the spec lines for it.
fn write_patch(ctx: &RunCtx, diff: &str) {
    let bytes = crate::bugfix::with_fix_header(
        diff.as_bytes(),
        &HeaderFacts {
            refs: refs(),
            upstream: None,
            status: UpstreamStatus::Merged,
            reason: "check the length".into(),
        },
    );
    std::fs::write(ctx.checkout_dir.join(NAME), bytes).unwrap();
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap();
    if !spec.contains(&format!("Patch0:")) {
        let out = crate::bugfix::spec_adding_patch(&spec, COMMENT, NAME).unwrap();
        std::fs::write(&ctx.spec_path, out).unwrap();
    }
}

/// A check that applies a patch unless it holds `BROKEN`.
fn check_unless_broken() -> Box<Check> {
    Box::new(|ctx, _| {
        let spec = std::fs::read_to_string(&ctx.spec_path).unwrap();
        CheckReport {
            patches: crate::patch_check::declared_patches(&spec)
                .into_iter()
                .map(|(_, name)| {
                    let body =
                        std::fs::read_to_string(ctx.checkout_dir.join(&name)).unwrap_or_default();
                    PatchResult {
                        status: if body.contains("BROKEN") {
                            PatchStatus::Fails {
                                output: format!("Patch {name} does not apply"),
                                rejects: "--- f.rej\n-old\n+new\n".into(),
                            }
                        } else {
                            PatchStatus::Applies
                        },
                        name,
                        level: 1,
                        refreshed: None,
                    }
                })
                .collect(),
            not_run: None,
        }
    })
}

fn broken_job() -> FixJob {
    let mut p = prepared(NAME, patch_bytes(FIX_DIFF));
    let mut text = String::from_utf8(p.bytes).unwrap();
    text = text.replacen("Description:", "Description: BROKEN", 1);
    p.bytes = text.into_bytes();
    job(vec![p])
}

fn scripted(
    mut turns: Vec<Box<dyn FnMut(&RunCtx) -> Reply>>,
) -> impl FnMut(&RunCtx, &str) -> Reply {
    let mut at = 0;
    move |ctx, _| {
        let t = turns.get_mut(at).expect("more turns than scripted");
        at += 1;
        t(ctx)
    }
}

fn writing(diff: &'static str) -> Box<dyn FnMut(&RunCtx) -> Reply> {
    Box::new(move |ctx| {
        write_patch(ctx, diff);
        Ok(fixed())
    })
}

fn said(a: crate::agent::TurnOutcome) -> Box<dyn FnMut(&RunCtx) -> Reply> {
    let a = std::cell::RefCell::new(Some(a));
    Box::new(move |_| Ok(a.borrow_mut().take().expect("said once")))
}

#[test]
fn without_an_upstream_patch_the_turn_writes_one_and_it_is_flagged_as_written() {
    let mut turns = scripted(vec![writing(FIX_DIFF)]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 1);
    assert!(
        case.contexts[0]
            .contains("Staged upstream patch: none: find the upstream fix or write one"),
        "{}",
        case.contexts[0]
    );
    assert!(case.contexts[0].contains(&format!("Patch file name to use: {NAME}")));
    assert_eq!(
        case.events.last().unwrap().applied.as_deref(),
        Some("written")
    );
    assert_eq!(case.reviews.len(), 1);
    assert!(
        case.reviews[0].contains("there is no upstream fix"),
        "{}",
        case.reviews[0]
    );
    // One bullet, the pipeline's, with the references.
    let bullet = format!("- Add {NAME}: the length is checked (bsc#1234567, CVE-2025-1234)");
    assert_eq!(case.changes.matches(NAME).count(), 1, "{}", case.changes);
    assert!(case.changes.contains(&bullet), "{}", case.changes);
}

#[test]
fn a_staged_patch_quilt_rejects_goes_to_the_turn_with_the_rejects_and_is_adapted() {
    let mut turns = scripted(vec![writing(FIX_DIFF)]);
    let case = Rig::new().run_turns(
        broken_job(),
        &mut *check_unless_broken(),
        &mut turns,
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    let context = &case.contexts[0];
    assert!(
        context.contains("Staged upstream patch: foo-CVE-2025-1234.patch\n<untrusted-data source=\"staged patch\">"),
        "{context}"
    );
    assert!(
        context.contains("does not apply") && context.contains("+new"),
        "{context}"
    );
    assert!(context.contains("<untrusted-data source=\"quilt rejects\">"));
    assert_eq!(
        case.events.last().unwrap().applied.as_deref(),
        Some("adapted")
    );
    assert!(
        case.reviews[0].contains("adapted by a bug fix turn"),
        "{}",
        case.reviews[0]
    );
}

#[test]
fn rounds_the_gate_and_the_answer_refuse_end_in_a_fix_that_does_not_apply() {
    let mut turns = scripted(vec![
        writing(BAD_DIFF),
        said(answer(Action::NoChange, FixVerdict::Fixed, "done", &[])),
        writing("--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n y\n-c\n+C\n d\n"),
    ]);
    let case = Rig::new().run_turns(
        broken_job(),
        &mut *check_unless_broken(),
        &mut turns,
        &mut |_, _| panic!("no review"),
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::FixDoesNotApply);
    assert!(reason.contains("after 3 rounds"), "{reason}");
    assert_eq!(case.contexts.len(), 3);
    assert!(
        case.contexts[1].contains("Your last edit was rolled back"),
        "{}",
        case.contexts[1]
    );
    assert!(case.contexts[1].contains("does not apply") || case.contexts[1].contains("patch"));
    assert!(case.contexts[2].contains("you answered fixed but changed nothing"));
    // The checkout is the pre-stage one.
    assert_eq!(case.spec, Rig::new().spec);
    assert!(!case.checkout.join(NAME).exists());
    assert!(!case.changes.contains(NAME));
}

#[test]
fn without_a_staged_patch_the_same_rounds_end_with_no_fix() {
    let mut turns = scripted(vec![
        said(answer(Action::NoChange, FixVerdict::Fixed, "a", &[])),
        said(answer(Action::NoChange, FixVerdict::Fixed, "b", &[])),
        said(answer(Action::NoChange, FixVerdict::Fixed, "c", &[])),
    ]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut |_, _| panic!("no review"),
    );
    assert_eq!(*failed(&case).0, FailureKind::NoFix);
}

#[test]
fn not_affected_needs_evidence_and_then_ends_the_package_as_no_fix() {
    let mut turns = scripted(vec![
        said(answer(
            Action::NoChange,
            FixVerdict::NotAffected,
            "the code is not here",
            &[],
        )),
        said(answer(
            Action::NoChange,
            FixVerdict::NotAffected,
            "the code is not here",
            &["src/parse.c has no parse_token"],
        )),
    ]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut |_, _| panic!("no review"),
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NoFix);
    assert_eq!(reason, "the code is not here");
    assert_eq!(case.contexts.len(), 2);
    assert!(
        case.contexts[1].contains("quote the code"),
        "{}",
        case.contexts[1]
    );
    assert_eq!(
        case.events.last().unwrap().verdict.as_deref(),
        Some("not_affected")
    );
}

#[test]
fn a_blocked_turn_needs_a_maintainer_and_leaves_the_checkout_as_it_was() {
    let mut turns = scripted(vec![Box::new(|ctx| {
        // A speculative file the blocked answer must not keep.
        std::fs::write(ctx.checkout_dir.join("junk.patch"), "x").unwrap();
        let mut out = answer(
            Action::Blocked {
                why: "it changes the ABI".into(),
            },
            FixVerdict::NeedsMaintainer,
            "it changes the ABI",
            &[],
        );
        out.action = Action::Blocked {
            why: "it changes the ABI".into(),
        };
        Ok(out)
    })]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut |_, _| panic!("no review"),
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NeedsMaintainer);
    assert!(reason.contains("ABI"), "{reason}");
    assert!(!case.checkout.join("junk.patch").exists());
}

#[test]
fn a_wheel_switch_is_no_answer_to_a_bug() {
    let mut out = answer(Action::NoChange, FixVerdict::NoFix, "x", &[]);
    out.action = Action::UseUpstreamWheel {
        why: "unbuildable from source here".into(),
    };
    let mut turns = scripted(vec![said(out), writing(FIX_DIFF)]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(case.contexts[1].contains("use_upstream_wheel is not a bug fix"));
    assert!(!case.checkout.join("wheel-origin.txt").exists());
}

#[test]
fn a_turn_error_ends_the_package_with_its_kind_and_restores_the_checkout() {
    let mut turns = scripted(vec![Box::new(|ctx| {
        write_patch(ctx, FIX_DIFF);
        Err(("tampered with osc metadata".into(), FailureKind::Tampered))
    })]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut |_, _| panic!("no review"),
    );
    assert_eq!(*failed(&case).0, FailureKind::Tampered);
    assert_eq!(case.spec, Rig::new().spec);
    assert!(!case.checkout.join(NAME).exists());
}

#[test]
fn no_turn_starts_with_the_budget_spent() {
    let mut rig = Rig::new();
    rig.spent = 100.0;
    let case = rig.run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut |_, _| panic!("no turn"),
        &mut |_, _| panic!("no review"),
    );
    assert_eq!(*failed(&case).0, FailureKind::Exhausted);
}

#[test]
fn without_the_agent_a_failing_staged_patch_needs_a_maintainer() {
    let mut rig = Rig::new();
    rig.no_agent = true;
    let case = rig.run(broken_job(), &mut *check_unless_broken(), &mut |_, _| {
        panic!("no review")
    });
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NeedsMaintainer);
    assert!(reason.contains("--no-agent"), "{reason}");
    assert_eq!(case.spec, Rig::new().spec);
    // And nothing to stage without the agent.
    let mut rig = Rig::new();
    rig.no_agent = true;
    let case = rig.run(job(Vec::new()), &mut *check_unless_broken(), &mut *accept());
    assert_eq!(*failed(&case).0, FailureKind::NeedsMaintainer);
}

#[test]
fn a_fix_that_adds_two_patches_is_refused_and_retried() {
    let mut turns = scripted(vec![
        Box::new(|ctx| {
            write_patch(ctx, FIX_DIFF);
            std::fs::write(ctx.checkout_dir.join("two.patch"), "x").unwrap();
            let spec = std::fs::read_to_string(&ctx.spec_path).unwrap();
            let spec = crate::bugfix::spec_adding_patch(&spec, COMMENT, "two.patch").unwrap();
            std::fs::write(&ctx.spec_path, spec).unwrap();
            Ok(fixed())
        }),
        writing(FIX_DIFF),
    ]);
    let case = Rig::new().run_turns(
        job(Vec::new()),
        &mut *check_unless_broken(),
        &mut turns,
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 2);
    assert!(!case.checkout.join("two.patch").exists());
}

fn effect(e: TurnEffect) -> Result<TurnEffect, crate::pipeline::TurnError> {
    Ok(e)
}

#[test]
fn every_row_of_the_answer_table_makes_its_move() {
    let m = |o: &crate::agent::TurnOutcome, e: Result<TurnEffect, _>, r: Option<&str>| {
        fix_move(o, &e, r)
    };
    let evid = ["code"];
    // edited + fixed: accepted, or refused by the gate.
    assert_eq!(
        m(&fixed(), effect(TurnEffect::Edited), None),
        FixMove::PostChecks
    );
    assert_eq!(
        m(
            &fixed(),
            effect(TurnEffect::NoChange),
            Some("a patch does not apply")
        ),
        FixMove::Retry("a patch does not apply".into())
    );
    // edited + any other verdict: rolled back and retried.
    for v in [
        FixVerdict::AlreadyFixed,
        FixVerdict::NotAffected,
        FixVerdict::NoFix,
    ] {
        let o = answer(Action::Edited, v, "r", &evid);
        assert!(
            matches!(m(&o, effect(TurnEffect::Edited), None), FixMove::Retry(w) if w.contains("you edited but answered"))
        );
    }
    // no_change + already_fixed / not_affected: evidence or retry.
    for (v, name) in [
        (FixVerdict::AlreadyFixed, "already_fixed"),
        (FixVerdict::NotAffected, "not_affected"),
    ] {
        let o = answer(Action::NoChange, v, "r", &evid);
        assert_eq!(
            m(&o, effect(TurnEffect::NoChange), None),
            FixMove::NoFix {
                verdict: name,
                reason: "r".into()
            }
        );
        let bare = answer(Action::NoChange, v, "r", &[" "]);
        assert!(
            matches!(m(&bare, effect(TurnEffect::NoChange), None), FixMove::Retry(w) if w.contains("evidence"))
        );
    }
    // no_change + no_fix.
    let o = answer(Action::NoChange, FixVerdict::NoFix, "searched", &[]);
    assert_eq!(
        m(&o, effect(TurnEffect::NoChange), None),
        FixMove::NoFix {
            verdict: "no_fix",
            reason: "searched".into()
        }
    );
    // blocked, whatever it says.
    let b = answer(
        Action::Blocked { why: "abi".into() },
        FixVerdict::Fixed,
        "r",
        &[],
    );
    assert_eq!(
        m(&b, Err("x".to_string().into()), None),
        FixMove::Maintainer("abi".into())
    );
    let n = answer(
        Action::Edited,
        FixVerdict::NeedsMaintainer,
        "version update",
        &[],
    );
    assert_eq!(
        m(&n, effect(TurnEffect::Edited), None),
        FixMove::Maintainer("version update".into())
    );
    // no_change + fixed: nothing was changed.
    let o = answer(Action::NoChange, FixVerdict::Fixed, "r", &[]);
    assert!(
        matches!(m(&o, effect(TurnEffect::NoChange), None), FixMove::Retry(w) if w.contains("changed nothing"))
    );
    // a missing fix field.
    let mut none = fixed();
    none.bug_fix = None;
    assert!(matches!(
        m(&none, effect(TurnEffect::Edited), None),
        FixMove::Retry(_)
    ));
}

#[test]
fn a_turn_that_changes_more_than_the_fix_is_refused_and_asked_again() {
    let mut turns = scripted(vec![
        Box::new(|ctx| {
            write_patch(ctx, FIX_DIFF);
            std::fs::write(ctx.checkout_dir.join("junk.txt"), "x").unwrap();
            Ok(fixed())
        }),
        writing(FIX_DIFF),
    ]);
    let case = Rig::new().run_turns(
        broken_job(),
        &mut *check_unless_broken(),
        &mut turns,
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 2);
    assert!(
        case.contexts[1].contains("it also changed the new file junk.txt"),
        "{}",
        case.contexts[1]
    );
    assert!(!case.checkout.join("junk.txt").exists());
    // Only the fix was reviewed.
    assert_eq!(case.reviews.len(), 1);
}

#[test]
fn a_turn_error_is_recorded_with_a_verdict_that_says_what_ended_it() {
    for (kind, verdict) in [
        (FailureKind::Tampered, "failed"),
        (FailureKind::RateLimited, "exhausted"),
        (FailureKind::AgentUnavailable, "exhausted"),
        (FailureKind::NeedsMaintainer, "needs_maintainer"),
    ] {
        let mut turns = scripted(vec![Box::new(move |_| Err(("boom".into(), kind)))]);
        let case = Rig::new().run_turns(
            job(Vec::new()),
            &mut *check_unless_broken(),
            &mut turns,
            &mut |_, _| panic!("no review"),
        );
        assert_eq!(*failed(&case).0, kind);
        assert_eq!(
            case.events.last().unwrap().verdict.as_deref(),
            Some(verdict),
            "{kind:?}"
        );
    }
}
