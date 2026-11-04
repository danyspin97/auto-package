//! The review turn's wanted-ness and context, and the wheel switch the pipeline
//! makes itself for a source archive without its git submodules.

use super::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::python_spec::*;

/// Whether the review turn runs for a package nobody asked to patch. A spec
/// generated from a CUDA redistributable is never reviewed: it has no source to
/// read, and the turn can only add a "no test suite" comment.
///
/// `checkout` holds the OBS status and whether OBS built exactly the spec on
/// disk (see `CheckoutFacts::exact_success`). An update that OBS already built
/// needs no edit. An update OBS rejected or failed is reviewed as the carried
/// recipe it is. A package that is not an update is reviewed unless `failed_log`
/// says OBS failed it and its log is there for the pre-fix turn.
pub(super) fn review_turn_wanted(
    need_spec: bool,
    update: bool,
    checkout: &CheckoutFacts,
    failed_log: bool,
    already_reviewed: bool,
    rpm_name: &str,
) -> bool {
    if already_reviewed || crate::redist::is_redist_package(rpm_name) {
        return false;
    }
    if need_spec {
        return true;
    }
    if update {
        return !checkout.exact_success();
    }
    // A failed build with its log is what the pre-fix turn works from; without
    // one, nothing else would look at the spec before the first build.
    !failed_log
}

/// The context of the review turn: a patch request alone, else every note
/// that applies (the empty submodules of a source archive, the carried recipe),
/// so that one note does not hide another.
pub(super) fn review_turn_context(
    patch: Option<&str>,
    submodules: Option<&str>,
    carried: Option<&str>,
) -> Option<String> {
    if let Some(patch) = patch {
        return Some(patch.to_string());
    }
    let notes: Vec<&str> = [submodules, carried].into_iter().flatten().collect();
    (!notes.is_empty()).then(|| notes.join("\n\n"))
}

/// Directories under `<root>/third_party` (or `3rdparty`, `thirdparty`) that
/// are empty: a git submodule a GitHub archive does not contain. A tree with
/// several of them (torch: pybind11, XNNPACK, ...) cannot be built from that
/// archive, and no spec edit changes that.
pub(super) fn empty_submodule_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for parent in ["third_party", "3rdparty", "thirdparty"] {
        let Ok(rd) = std::fs::read_dir(root.join(parent)) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir()
                && std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_none())
                && let Some(name) = e.file_name().to_str()
            {
                out.push(format!("{parent}/{name}"));
            }
        }
    }
    out.sort();
    out
}

/// Review-turn context when the source archive lacks its git submodules, or
/// `None`. Two empty directories are the threshold: one can be a placeholder.
pub(super) fn empty_submodule_context(root: &Path) -> Option<String> {
    let empty = empty_submodule_dirs(root);
    (empty.len() >= 2).then(|| {
        format!(
            "The source archive is missing its git submodules: {} are empty directories. \
             No spec edit supplies them, so do not try: this tree cannot be built from \
             this archive. Return `use_upstream_wheel` now, quoting these directories as the \
             reason, if the project publishes a wheel; otherwise report the dependency that fixes it in `patch_dependencies` or vendor the submodules with a `_service` (never `blocked`).",
            empty.join(", ")
        )
    })
}

/// The `why` the pipeline gives its own wheel switch for an archive without
/// its git submodules.
pub(super) fn empty_submodule_why(root: &Path) -> String {
    format!(
        "the source archive is missing its git submodules ({} are empty directories), \
         so it cannot be built from source",
        empty_submodule_dirs(root).join(", ")
    )
}

/// A `use_upstream_wheel` outcome made by the pipeline itself, so the switch goes
/// through the same path (provenance, notes, state reset) as the agent's.
pub(super) fn pipeline_wheel_outcome(why: &str) -> crate::agent::TurnOutcome {
    crate::agent::TurnOutcome {
        action: crate::agent::Action::UseUpstreamWheel {
            why: why.to_string(),
        },
        reasoning: why.to_string(),
        changelog: String::new(),
        evidence: Vec::new(),
        unresolvable_packages: Vec::new(),
        patch_dependencies: Vec::new(),
        stage_files: Vec::new(),
        patch_answers: Vec::new(),
        patch_review: None,
        bug_fix: None,
        review_findings: Vec::new(),
        finding_answers: Vec::new(),
        changes_entry: String::new(),
        confidence: crate::agent::Confidence::High,
        usage: crate::agent::Usage::default(),
        tool_calls: Vec::new(),
        prompt: String::new(),
        output: String::new(),
        raw: String::new(),
        session_id: None,
        ended_early: None,
        denied_commands: Vec::new(),
    }
}

/// A source archive without its git submodules cannot be built by any spec
/// edit: when the wheel exists, switch to it now instead of paying a turn to be
/// told so. `true` when the spec was switched (the Review turn is then not
/// needed); on a failed switch the Review turn decides.
pub(super) fn switch_for_missing_submodules(
    ctx: &RunCtx,
    st: &mut RunState,
    spec_path: &Path,
    lookup: PypiLookup<'_>,
) -> Result<bool, PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let mut switched = false;
    if st.from_repo_archive
        && let root = ws.sdist_dir().join(&st.sdist_top_level)
        && empty_submodule_context(&root).is_some()
    {
        let why = empty_submodule_why(&root);
        let step = begin_or_fail!(
            ctx.pkg_run,
            *reporter,
            StepKind::AgentTurn,
            Some("submodule-wheel"),
            &mut st.step_attempts,
            rpm_name.clone()
        );
        let before = std::fs::read_to_string(spec_path).unwrap_or_default();
        let outcome = pipeline_wheel_outcome(&why);
        let applied = apply_turn_and_reset_with(
            ApplyTurnCtx {
                config,
                checkout_dir,
                spec_path,
                step: &step,
                before: &before,
                rpm_name,
                label: "Review — pre-build (git submodules missing)",
                attempt: 0,
                notes_path: ws.notes(),
                snapshot: None,
                kept: &[],
                staged_out: None,
                rejected_out: None,
            },
            &outcome,
            *reporter,
            &mut st.sdist_top_level,
            &mut st.from_repo_archive,
            lookup,
        );
        match applied {
            Ok(TurnEffect::SwitchedToWheel) => {
                step.finish(ap_core::event::StepOutcome::Ok);
                switched = true;
            }
            Err(e) if e.ends_package() => {
                step.finish(ap_core::event::StepOutcome::Failed);
                return Err(e.into_failed(ctx));
            }
            other => {
                step.finish(ap_core::event::StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: None,
                    message: format!(
                        "{rpm_name}: the source archive lacks its git submodules and the \
                         wheel switch did not happen ({}); the Review turn decides",
                        other.err().map_or_else(|| "no change".into(), |e| e.reason)
                    ),
                });
            }
        }
    }
    Ok(switched)
}
