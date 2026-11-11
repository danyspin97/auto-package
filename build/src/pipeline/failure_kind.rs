//! Why a package failed: the kinds a pipeline stage returns, and the rules
//! that depend on them. Callers decide from the kind, never from the reason.

/// Why a package failed. Callers decide what to retry, count and give up on
/// from this, never from the wording of `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureKind {
    #[default]
    Other,
    /// The API answered 429 and the shared pause did not clear it.
    RateLimited,
    /// The package used up its budget or its attempt allowance.
    Exhausted,
    /// Another process held the package's build lock past the wait. Nothing
    /// was tried, so the package is deferred rather than failed.
    Busy,
    /// The run was aborted because the agent cannot work at all (claude is not
    /// logged in). Not a package failure: nothing is retried or counted.
    AgentUnavailable,
    /// The network, OBS or this machine failed (a dropped connection, an OBS
    /// 5xx, a timeout), not the package. Recorded as `interrupted`, so the
    /// package reads as re-runnable; the caller retries it within its usual
    /// bound.
    Transient,
    /// The disk is full (`ENOSPC`). Handled like `Transient`, and told apart
    /// so the user can free space and retry the package.
    DiskFull,
    /// A patch turn ran but did not make the dependent's shipping condition
    /// true. The package's own condition counters bound the repeats (a second
    /// patch turn, then the upstream wheel), so the caller re-queues the
    /// request within the same run whatever its round count.
    ConditionUnmet,
    /// The agent's turn itself failed: the process crashed or timed out, or its
    /// answer could not be read. Not a refusal of the package's edit (the apply
    /// gate's refusals are `Other`), and a style-only failure downgrades it like
    /// the other agent-side kinds.
    AgentFailed,
    /// The agent's turn changed the checkout's osc or git metadata, or left a
    /// link that leaves the tree (see `agent::integrity`). Its own kind: no
    /// caller downgrades it to a warning or retries it, because the checkout
    /// may not be as the pipeline left it. A change that could not be put back
    /// is named in the reason.
    Tampered,
    /// The new release needs a decision a maintainer makes, such as a signing
    /// key the package's keyring lacks. Nothing was sent to OBS, and nothing is
    /// retried: the same refusal would come back.
    NeedsMaintainer,
    /// OBS does not build the package for this repository and arch (excluded
    /// or disabled there). Recorded as a skip with the reason, not a failure.
    Excluded,
    /// A source the package needs is gone upstream: its URL answers 404 or
    /// 410, so the release moved or was removed outside the tool (class 4 of
    /// D-06). Nothing is retried: the same answer would come back.
    BrokenOutside,
    /// The spec's `Version:` is not the package's before or after a turn: it
    /// was changed between turns, or by a turn that may not change it. The
    /// spec is put back and the package stops; no caller downgrades this.
    VersionChanged,
    /// `auto-package fix` found that the bug needs no fix here (the fix is in
    /// the source already, or the package is not affected) or found no fix:
    /// the reason says which. Nothing is retried.
    NoFix,
    /// The upstream fix could not be made to apply to this version. Nothing is
    /// retried.
    FixDoesNotApply,
    /// The machine the package builds on cannot run it: a tool it needs is
    /// missing, or a toolchain has no default. Not a defect of the package,
    /// and a rerun here repeats it until the machine changes. It is given up
    /// at once by the frontier (never retried), never starts an agent turn,
    /// and never takes the identical-failure stop. Otherwise it reads as
    /// `Other`.
    Environment,
}

/// Whether an aborted run reports a failure of this kind as the abort: what
/// was in flight died of the abort (killed children, refused spawns), so it
/// is not the package's own failure. The kinds left out are ones an abort
/// cannot have caused: a maintainer's decision, a deferral, a skip, a spent
/// budget.
pub(super) fn reported_as_abort(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::Other
            | FailureKind::AgentUnavailable
            | FailureKind::AgentFailed
            | FailureKind::Transient
            | FailureKind::DiskFull
            | FailureKind::ConditionUnmet
            | FailureKind::Environment
    )
}

/// Whether a ladder that ended in `kind` declines the patch request it was
/// run for, so the dependent does not ask for the same patch again. An
/// environment failure does not decline it: the request is asked again once
/// the machine can run the ladder.
pub(super) fn declines_request_after_ladder(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::Exhausted
            | FailureKind::Other
            | FailureKind::AgentFailed
            | FailureKind::NeedsMaintainer
    )
}

/// Whether a patch turn that failed with `kind` declines its request (the
/// package-side kinds only). Environment is left out for the same reason as
/// `declines_request_after_ladder`.
pub(super) fn declines_request_after_turn(kind: FailureKind) -> bool {
    matches!(kind, FailureKind::Other | FailureKind::AgentFailed)
}

/// Whether a spec-style fix that failed with `kind` is only a warning (the
/// package proceeds with its spec as it is). Budget exhaustion and the apply
/// gate's refusals are not in this list, so they fail the package.
pub(super) fn style_failure_warns(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::AgentUnavailable
            | FailureKind::AgentFailed
            | FailureKind::RateLimited
            | FailureKind::Busy
            | FailureKind::Transient
            | FailureKind::DiskFull
            | FailureKind::Exhausted
    )
}

/// The verdict a fix or findings round records when its agent turn failed
/// with `kind`. Only a maintainer's decision is `needs_maintainer`; the
/// budget, quota and agent-side kinds are `exhausted`; the rest are `failed`.
pub(super) fn turn_failure_verdict(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::NeedsMaintainer => "needs_maintainer",
        FailureKind::RateLimited | FailureKind::AgentUnavailable | FailureKind::Exhausted => {
            "exhausted"
        }
        _ => "failed",
    }
}
