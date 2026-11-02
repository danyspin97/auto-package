//! The rpmlint round: a green build whose rpmlint still reports `E:` lines
//! gets one fix turn with those lines, while the attempt budget lasts. A line
//! is asked about once per package version (see [`rpmlint_key`]). The edit the
//! turn makes is built again, and the spec that built comes back if that build
//! does not succeed, so a green package never ends failed because of this round.

use super::*;
use crate::pipeline::apply_turn::TurnEffect;
use crate::pipeline::logs::link_build_log;
use crate::pipeline::prompt_notes::persist_attempts;
use crate::pipeline::snapshot::CheckoutSnapshot;
use crate::pipeline::spec_edits::{restore_spec, strip_and_rewrite};
use std::collections::BTreeSet;

/// The context the fix turn is given before the lines. The task text says the
/// build succeeded; the lines are rpmlint's findings on its RPMs.
const RPMLINT_CONTEXT: &str = "The build SUCCEEDED. rpmlint, run on the RPMs it built, reported the \
     `E:` lines below. rpmlintrc filters are already applied, so a filtered line is not listed. \
     Fix the cause in the spec (convention 39: fix first; a filter only for what cannot be fixed, \
     with a `# reason:` line). A package's own installed headers belong in a -devel subpackage \
     (convention 53). If you judge a line cannot be fixed, leave it: it is not asked about again \
     for this package version.";

/// What the rpmlint round needs to know before its turn.
pub(super) struct RpmlintGate {
    pub(super) no_agent: bool,
    /// `--local`: there is no OBS rpmlint log to act on.
    pub(super) local: bool,
    pub(super) attempts: usize,
    pub(super) max_attempts: usize,
    pub(super) spent_usd: f64,
    pub(super) budget_usd: f64,
}

/// Whether the round asks the fix turn about the build's `E:` lines.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RpmlintAsk {
    /// Build as it is: no line is new, or no turn is possible under the guards.
    Build,
    /// New lines, but no attempt or budget is left: build and warn.
    Spent,
    /// Ask the turn about these new lines.
    Ask(Vec<String>),
}

/// Decide whether the `E:` lines of a green build get a fix turn. Lines whose
/// key is in `shown` were asked about already in this package version.
pub(super) fn rpmlint_ask(
    gate: &RpmlintGate,
    errors: &[String],
    shown: &BTreeSet<String>,
) -> RpmlintAsk {
    if gate.no_agent || gate.local {
        return RpmlintAsk::Build;
    }
    let fresh: Vec<String> = errors
        .iter()
        .filter(|e| !shown.contains(&rpmlint_key(e)))
        .cloned()
        .collect();
    if fresh.is_empty() {
        RpmlintAsk::Build
    } else if gate.attempts < gate.max_attempts && gate.spent_usd < gate.budget_usd {
        RpmlintAsk::Ask(fresh)
    } else {
        RpmlintAsk::Spent
    }
}

/// What the round does with the turn's effect.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RpmlintTurn {
    /// The spec that built stands: the turn changed nothing, or its edit could
    /// not be used. The package is built and this warning is reported.
    Keep(String),
    /// The turn edited the spec: it is built again, and the spec that built is
    /// restored if that build does not succeed.
    Rebuild,
}

/// Decide what a fix turn's result means for the green build. `effect` is the
/// turn's effect, or why it could not be used (its edit was then rolled back).
pub(super) fn rpmlint_after_turn(
    effect: Result<TurnEffect, String>,
    rpm_name: &str,
    errors: &[String],
) -> RpmlintTurn {
    match effect {
        Ok(TurnEffect::Edited | TurnEffect::SwitchedToWheel) => RpmlintTurn::Rebuild,
        Ok(TurnEffect::NoChange) => RpmlintTurn::Keep(rpmlint_warning(
            rpm_name,
            "the fix turn changed nothing",
            errors,
        )),
        Err(why) => RpmlintTurn::Keep(rpmlint_warning(
            rpm_name,
            &format!("the fix turn could not be applied ({why}); the spec that built stands"),
            errors,
        )),
    }
}

/// The warning for a package shipped with rpmlint errors: `why`, then the
/// error lines.
pub(super) fn rpmlint_warning(rpm_name: &str, why: &str, errors: &[String]) -> String {
    format!(
        "{rpm_name}: shipped with rpmlint errors, {why}: {}",
        errors.join("; ")
    )
}

/// The identity of an rpmlint error line for the once-per-version rule: its
/// rule and its path (or detail), with digit runs blurred, so `python3.13` and
/// `python3.11`, or `lib64` and `lib`, are the same. The subpackage, the
/// badness and the architecture are left out: a line that only moves to
/// another subpackage is the same defect.
pub(super) fn rpmlint_key(line: &str) -> String {
    let rest = line.split_once(": E: ").map_or(line, |(_, r)| r);
    // Drop the `(Badness: N)` group, which changes with the rpmlint version.
    let rest = match rest.find("(Badness:") {
        Some(i) => format!(
            "{}{}",
            &rest[..i],
            rest[i..].split_once(')').map_or("", |(_, after)| after)
        ),
        None => rest.to_string(),
    };
    let mut words = rest.split_whitespace();
    let rule = words.next().unwrap_or_default();
    let detail: Vec<&str> = words.collect();
    let subject = detail
        .iter()
        .find(|w| w.starts_with('/'))
        .map_or_else(|| detail.join(" "), |w| w.to_string());
    format!("{rule} {}", blur_digits(&subject))
}

/// The lines of `errors` that the origin's own build does not report either,
/// by [`rpmlint_key`]. A line the origin already has is the origin's defect:
/// an update does not fix it, so it does not start a turn.
pub(super) fn errors_not_in_origin(errors: &[String], origin: &[String]) -> Vec<String> {
    let known: BTreeSet<String> = origin.iter().map(|e| rpmlint_key(e)).collect();
    errors
        .iter()
        .filter(|e| !known.contains(&rpmlint_key(e)))
        .cloned()
        .collect()
}

/// The `E:` lines of the origin's latest build log for the same repository
/// and arch. Empty when the package is not a link, or the log cannot be read:
/// then no line is excused and the turn is asked as before.
fn origin_rpmlint_errors(config: &PipelineConfig, obs_name: &str) -> Vec<String> {
    let origin = match osc::link_origin(&config.project, obs_name, Emit::quiet()) {
        Ok(Some(origin)) => origin,
        _ => return Vec::new(),
    };
    osc::fetch_build_log_tail(
        &origin.project,
        &origin.package,
        &config.distro,
        &config.arch,
    )
    .map(|log| osc::rpmlint_errors(&log))
    .unwrap_or_default()
}

/// `errors` without the lines the origin's build reports too. The origin is
/// asked only when there is a line to excuse and a turn could be asked.
fn errors_to_ask(ctx: &RunCtx, build_step: &StepDir, errors: &[String]) -> Vec<String> {
    let config = ctx.config;
    if errors.is_empty() || config.no_agent || osc::local_build().is_some() {
        return errors.to_vec();
    }
    let origin = origin_rpmlint_errors(config, &ctx.obs_name);
    let fresh = errors_not_in_origin(errors, &origin);
    if fresh.len() < errors.len() {
        let excused: Vec<String> = errors
            .iter()
            .filter(|e| !fresh.contains(e))
            .cloned()
            .collect();
        ctx.reporter.report(&Event::Progress {
            step: Some(build_step.id),
            message: format!(
                "{}: {} rpmlint error(s) the origin's build reports too, not asked of the \
                 update: {}",
                ctx.rpm_name,
                excused.len(),
                excused.join("; ")
            ),
        });
    }
    fresh
}

/// `text` with every run of digits replaced by `#`.
fn blur_digits(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    out
}

/// The keys of this package version's shown lines, as persisted. A state file
/// for another version, or one without the fields, gives none.
pub(super) fn load_rpmlint_shown(prior: &PkgState, version: &str) -> BTreeSet<String> {
    if prior.rpmlint_shown_version.as_deref() == Some(version) {
        prior.rpmlint_shown.iter().cloned().collect()
    } else {
        BTreeSet::new()
    }
}

/// Save the shown keys with the version they belong to, so a resumed run does
/// not ask about the same lines again.
pub(super) fn persist_rpmlint_shown(ws: &Workspace, version: &str, shown: &BTreeSet<String>) {
    let saved = PkgState::update(ws, |st| {
        st.rpmlint_shown_version = Some(version.to_string());
        st.rpmlint_shown = shown.iter().cloned().collect();
    });
    if let Err(e) = saved {
        ap_core::diag!("  cannot save the rpmlint lines already shown: {e}");
    }
}

/// The fix turn for the rpmlint errors of a successful build. `Some` is the
/// round to run next: the turn edited the spec, so it is built again and
/// [`revert_post_review`](super::build_round) puts the built spec back if that
/// build fails. `None` builds the package as it is, with a warning when the
/// errors remain and were not asked about.
pub(super) fn rpmlint_fix_round(
    ctx: &RunCtx,
    st: &mut RunState,
    build_step: &StepDir,
    build_log: &Path,
    all_errors: &[String],
) -> Option<RoundEnd> {
    let errors = &errors_to_ask(ctx, build_step, all_errors);
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    let gate = RpmlintGate {
        no_agent: config.no_agent,
        local: osc::local_build().is_some(),
        attempts: st.attempts,
        max_attempts: config.max_attempts,
        spent_usd: st.spent_usd,
        budget_usd: config.budget_usd,
    };
    let fresh = match rpmlint_ask(&gate, errors, &st.rpmlint_shown) {
        RpmlintAsk::Build => return None,
        RpmlintAsk::Spent => {
            reporter.report(&Event::Warning {
                step: Some(build_step.id),
                message: rpmlint_warning(rpm_name, "the fix attempts are spent", errors),
            });
            return None;
        }
        RpmlintAsk::Ask(fresh) => fresh,
    };
    st.rpmlint_shown
        .extend(errors.iter().map(|e| rpmlint_key(e)));
    persist_rpmlint_shown(ws, &config.version, &st.rpmlint_shown);
    st.attempts += 1;
    persist_attempts(ws, &config.run_id, st.attempts);
    let attempts = st.attempts;
    // The spec that built, and the checkout as it is now, to go back to if the
    // turn's edit is not what gets built.
    let good = strip_and_rewrite(spec_path);
    let snapshot = CheckoutSnapshot::take(checkout_dir);
    link_build_log(ws, build_log);
    let context = format!(
        "{RPMLINT_CONTEXT}\n\nrpmlint `E:` lines:\n{}",
        crate::agent::fenced_data(&fresh.join("\n"))
    );
    let effect = fix_turn(
        ctx,
        st,
        attempts,
        crate::agent::Task::RpmlintFix,
        None,
        Some(&context),
        Some(build_log),
    )
    .map_err(|e| turn_failure(&e));
    match rpmlint_after_turn(effect, rpm_name, errors) {
        RpmlintTurn::Keep(warning) => {
            snapshot.restore(reporter, Some(build_step));
            restore_spec(spec_path, &good, Some(build_step), reporter);
            reporter.report(&Event::Warning {
                step: Some(build_step.id),
                message: warning,
            });
            None
        }
        RpmlintTurn::Rebuild => {
            st.post_review_revert = Some(PostReviewRevert {
                spec: good,
                snapshot,
                kind: RevertKind::Rpmlint(errors.to_vec()),
            });
            Some(RoundEnd::Retry)
        }
    }
}

/// Why a fix turn's result could not be used, as the warning names it.
fn turn_failure(e: &PipelineResult) -> String {
    match e {
        PipelineResult::Failed { reason, .. } => reason.clone(),
        PipelineResult::MissingDeps { deps, .. } => {
            format!("it needs {} built first", deps.join(", "))
        }
        _ => "the turn gave no usable edit".to_string(),
    }
}
