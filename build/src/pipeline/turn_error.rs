//! Why a turn's outcome could not be applied, and how the package ends for it.

use super::*;

/// A turn outcome that could not be applied: the reason, and the kind the
/// package ends with when nothing downgrades it. A refusal that needs a person
/// (a wheel switch that would drop a security patch) says so here, so no caller
/// can fold it into an ordinary failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TurnError {
    pub(super) reason: String,
    pub(super) kind: FailureKind,
}

impl From<String> for TurnError {
    fn from(reason: String) -> Self {
        TurnError {
            reason,
            kind: FailureKind::Other,
        }
    }
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl TurnError {
    pub(super) fn needs_maintainer(reason: String) -> Self {
        TurnError {
            reason,
            kind: FailureKind::NeedsMaintainer,
        }
    }

    /// Whether the package must stop with this error: no rung of the ladder, no
    /// warning and no later turn is to absorb it.
    pub(super) fn ends_package(&self) -> bool {
        self.kind == FailureKind::NeedsMaintainer
    }

    /// The package's failure for this error.
    pub(super) fn into_failed(self, ctx: &RunCtx) -> PipelineResult {
        ctx.failed_as(self.kind, self.reason)
    }
}
