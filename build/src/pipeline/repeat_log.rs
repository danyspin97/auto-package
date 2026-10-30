//! The decisions for a build whose log repeats the last attempt's, and for the
//! extra build that tells a flaky test from a stable failure. Under `--local`
//! the log is the build that just ran, so nothing is asked of OBS.

use super::*;

/// What a build log that is the same as the last attempt's asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SameLogAction {
    /// On OBS the log may be the previous build handed back: a rebuild is asked
    /// for, once.
    AskObsRebuild,
    /// Under `--local` the log is the build that just ran: the agent reads it.
    Continue,
    /// The same log again: nothing the agent did changed the result.
    Fail,
}

/// The action for a log that repeats the last attempt's, the `repeats`-th time
/// in a row (1 for the first repeat).
pub(super) fn same_log_action(local: bool, repeats: usize) -> SameLogAction {
    if repeats > 1 {
        SameLogAction::Fail
    } else if local {
        SameLogAction::Continue
    } else {
        SameLogAction::AskObsRebuild
    }
}

/// How the extra build that tells a flaky test from a stable failure is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlakyRetry {
    ObsRebuild,
    LocalBuild,
}

pub(super) fn flaky_retry(local: bool) -> FlakyRetry {
    if local {
        FlakyRetry::LocalBuild
    } else {
        FlakyRetry::ObsRebuild
    }
}
