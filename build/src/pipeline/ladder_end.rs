//! How a spent resolution ladder ends the package: the short failure the user
//! reads, the full text kept beside it, the notes journal and the memo.

use super::*;
use crate::pipeline::ladder::{LADDER_FAILED_PREFIX, LadderSite};
use crate::pipeline::ladder_recap::{Recap, cap, first_sentence, recap};
use crate::pipeline::render::*;
use ap_core::failure_note::FailureNote;

/// Most characters of the failure's `reason`.
pub(super) const REASON_CAP: usize = 400;

/// What a resolver round said for the user, if it said anything.
pub(super) struct RoundAnswer {
    pub(super) round: usize,
    pub(super) summary: Option<String>,
    pub(super) action: Option<String>,
}

/// The step that follows a fix: a rerun of the package past the memo.
pub(super) fn force_hint(pkg: &str) -> String {
    format!("after fixing the cause, run: auto-package build {pkg} --force")
}

/// The text a rerun of the package is named by: the request as typed, else the
/// package's own name. None for a branch build, which no plain `build` repeats.
pub(super) fn rerun_target(config: &PipelineConfig) -> Option<&str> {
    (!config.branch_build).then(|| config.request_text.as_deref().unwrap_or(&config.pkg_name))
}

/// The action of a spent ladder: the stored step (`action`), then the command
/// that reruns the package with `--force` when there is a `target` to name.
/// Without one, only the stored step is left.
pub(super) fn rerun_action(action: Option<&str>, target: Option<&str>) -> Option<String> {
    match target {
        Some(target) => Some(then_step(action, &force_hint(target))),
        None => action.map(str::to_string),
    }
}

/// `first` (when there is one) and then `next`, as one action.
/// A sentence's own `.` or `;` after a letter or digit is dropped; one that is
/// part of a command or path (`cd .`, `./`) stays.
pub(super) fn then_step(first: Option<&str>, next: &str) -> String {
    let Some(first) = first else {
        return next.to_string();
    };
    let first = first.trim_end();
    let first = match first.strip_suffix(['.', ';']) {
        Some(rest) if rest.ends_with(char::is_alphanumeric) => rest,
        _ => first,
    };
    format!("{first}; {next}")
}

/// How the ladder ended, which decides what a rerun needs.
pub(super) enum Ending {
    /// Every rung ran and the memo was written: a rerun needs `--force`.
    Remembered,
    /// Every rung ran on a dependency being patched: no memo, nothing to force.
    Unremembered,
    /// The run's budget was spent before every rung ran.
    Budget { spent: f64, limit: f64 },
}

fn budget_text(spent: f64, limit: f64) -> String {
    format!("stopped on the budget (${spent:.2} spent of ${limit:.2}) before every rung ran")
}

/// The start of the failure's `reason`, before the recap.
fn failure_head(rpm: &str, ending: &Ending) -> String {
    match ending {
        Ending::Budget { spent, limit } => {
            format!(
                "{rpm} could not be resolved: {}.",
                budget_text(*spent, *limit)
            )
        }
        _ => format!("{rpm} could not be resolved:"),
    }
}

/// The start of the refusal of a spent spec, before the recap.
pub(super) fn memo_head(rpm: &str) -> String {
    format!("{rpm} could not be resolved (the same spec failed before):")
}

/// The recap's text that fits `head` and the reason's cap: whole trailing
/// lines go first, the first `pinned` never.
fn fit(head: &str, recap: &Recap, pinned: usize) -> String {
    recap.body_within(REASON_CAP.saturating_sub(head.chars().count() + 1), pinned)
}

/// The `reason` and the `action` of the failure: `recap` is what the user is
/// told (its first `pinned` lines are kept whole), `target` what the user
/// types after `auto-package build` to run the package again.
pub(super) fn short_failure(
    rpm: &str,
    target: Option<&str>,
    recap: &Recap,
    pinned: usize,
    ending: &Ending,
) -> (String, Option<String>) {
    let head = failure_head(rpm, ending);
    let reason = format!("{head} {}", fit(&head, recap, pinned));
    let reason = cap(reason.trim_end(), REASON_CAP);
    let action = recap.action.as_deref();
    let action = match ending {
        Ending::Remembered => rerun_action(action, target),
        Ending::Unremembered => action.map(str::to_string),
        Ending::Budget { .. } => Some(then_step(action, "raise --budget and run again")),
    };
    (reason, action)
}

/// The summary and action to recap: the last round that gave a summary, with
/// its action, else the last action any round gave.
fn resolver_text(answers: &[RoundAnswer]) -> (Option<&str>, Option<&str>) {
    let last = answers.iter().rev().find(|a| a.summary.is_some());
    let action = last
        .and_then(|a| a.action.as_deref())
        .or_else(|| answers.iter().rev().find_map(|a| a.action.as_deref()));
    (last.and_then(|a| a.summary.as_deref()), action)
}

/// The failure of a package whose ladder ran out of rungs (`history`) or of
/// budget (`budget_stopped`). Writes the notes journal and, for a ladder
/// that ran its rungs on the package itself, the memo.
pub(super) fn end_exhausted(
    ctx: &RunCtx,
    spent_usd: f64,
    site: &LadderSite<'_>,
    history: &[String],
    answers: &[RoundAnswer],
    budget_stopped: bool,
    wheel_refused: Option<String>,
) -> PipelineResult {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        spec_path,
        ..
    } = ctx;
    let text = history
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let who = if i == 0 { "turn" } else { "round" };
            format!("{who} {i}: {}", brief(r))
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let mut journal = history
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {}", i + 1, brief(r)))
        .collect::<Vec<_>>()
        .join("\n");
    for a in answers {
        for (what, value) in [("user_summary", &a.summary), ("user_action", &a.action)] {
            if let Some(value) = value {
                journal.push_str(&format!("\nround {} {what}: {value}", a.round));
            }
        }
    }
    append_notes(
        ws.notes(),
        &format!(
            "\n## {} — resolution ladder {}\n{LADDER_FAILED_PREFIX}{}:\n{journal}\n",
            site.apply_label,
            if budget_stopped {
                "stopped on the budget"
            } else {
                "spent"
            },
            if budget_stopped {
                "the run's budget was spent before every rung ran"
            } else {
                "every rung failed"
            },
        ),
    );
    // Every rung that ran failed and the wheel was refused for a security
    // patch: the package needs a maintainer, and so does a rerun of the same
    // spec (the memo keeps the kind and the refusal).
    let (kind, text) = match &wheel_refused {
        Some(refusal) if !budget_stopped => {
            (FailureKind::NeedsMaintainer, format!("{text}; {refusal}"))
        }
        _ => (FailureKind::Exhausted, text),
    };
    let (summary, action) = resolver_text(answers);
    let mut short = recap(
        history,
        summary,
        action,
        None,
        ap_core::remedy::host_distro(),
    );
    // The refusal is the most actionable line: it comes first, and is kept
    // whole when the reason is cut to its cap.
    let pinned = match wheel_refused.as_deref().filter(|_| !budget_stopped) {
        Some(refusal) => {
            short.lines.insert(0, first_sentence(refusal));
            1
        }
        None => 0,
    };
    let target = rerun_target(config);
    // A patch-driven ladder (the package is a dependency being patched)
    // writes no memo: the spec it failed on is the dependency's, and an
    // ordinary run of that package must not find it spent. `spent_ladder_refusal`
    // likewise skips the check for a patch run. A ladder the budget stopped did
    // not try its rungs, so nothing is remembered: a rerun with more budget
    // must run them.
    let remembered = should_remember_ladder(
        site.task == crate::agent::Task::PatchDependency,
        budget_stopped,
    );
    if remembered {
        // Keyed on the spec on disk now, which is what `spent_ladder_refusal`
        // reads: a round that kept spec edits (a deferred npm route) moved the
        // base on from `site.before`.
        let spec_now = std::fs::read_to_string(spec_path).unwrap_or_default();
        let key = ladder_key(config, &spec_now);
        let memo = text.clone();
        let memo_summary = fit(&memo_head(rpm_name), &short, pinned);
        let memo_action = short.action.clone();
        let needs = kind == FailureKind::NeedsMaintainer;
        update_state(ws, *reporter, |state| {
            state.ladder_spec_hash = Some(key);
            state.ladder_reason = Some(memo);
            state.ladder_needs_maintainer = needs;
            state.ladder_recap.ladder_summary = Some(memo_summary);
            state.ladder_recap.ladder_action = memo_action;
        });
    }
    let ending = if budget_stopped {
        Ending::Budget {
            spent: spent_usd,
            limit: config.budget_usd,
        }
    } else if remembered {
        Ending::Remembered
    } else {
        Ending::Unremembered
    };
    let (reason, action) = short_failure(rpm_name, target, &short, pinned, &ending);
    let full = match &ending {
        Ending::Budget { .. } => format!(
            "{LADDER_FAILED_PREFIX}{rpm_name}: {}: {text}",
            budget_text(spent_usd, config.budget_usd)
        ),
        _ => format!("{LADDER_FAILED_PREFIX}{rpm_name}: {text}"),
    };
    ctx.failed_note(
        kind,
        reason,
        FailureNote {
            detail: Some(full),
            action,
        },
    )
}
