//! What a fix attempt is allowed to look at, and in what order.
//!
//! The ladder is strict, and ordered by cost:
//!
//! 1. [`Level::ErrorContext`] — the extracted excerpt. ~428 tokens on run 1.
//! 2. [`Level::BuildTree`] — source files from the unpacked sdist.
//! 3. [`Level::BuildRoot`] — what actually got installed.
//! 4. [`Level::FullLog`] — the entire build log. ~34k tokens for a trivial
//!    `noarch` package and worse for anything that compiles, which is why it is
//!    last: it costs more than every other rung put together.
//!
//! The loop is iterative and the ladder resets with it. Send the error and its
//! small context, take the fix, rebuild if the model is confident — and if the
//! rebuild fails with a *different* error, start again at rung 1 with the new
//! excerpt rather than carrying the old height forward. Climbing is only for an
//! error that will not go away: a repeated signature is evidence that the
//! current rung did not contain the answer.
//!
//! So an [`Escalation`] is scoped to one error signature, not to a package or a
//! build. A new signature means a new `Escalation`.
//!
//! Three rules that are easy to get wrong:
//!
//! - **Nothing above rung 1 is read speculatively.** The excerpt answered run 1
//!   outright; inlining files on every attempt would spend most of the 81×
//!   saving the extractor buys, on attempts that never needed it.
//! - **`BUILDROOT` is not read until there is a failure.** It holds build
//!   *output*, so before a failure it answers nothing, and even after one it
//!   answers a narrow question — what got installed, versus what `%files`
//!   expected. Source explains causes; the buildroot shows consequences.
//! - **The whole log is a last resort, not a fallback.** Reaching for it means
//!   the extractor failed to find the signal, and that is worth recording as a
//!   defect in the extractor rather than absorbing quietly.

use std::fmt;

/// A rung on the context ladder.
///
/// Ordered cheapest-first: `ErrorContext < BuildTree < BuildRoot < FullLog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    /// The extracted error context. Where every attempt starts.
    #[default]
    ErrorContext,
    /// Source files from the unpacked sdist (`…/BUILD/<sdist-root>/`).
    BuildTree,
    /// Installed files from `…/BUILDROOT/`. Only after a failure.
    BuildRoot,
    /// The entire build log, unextracted. The most expensive thing we can send.
    FullLog,
}

impl Level {
    /// The next rung, or `None` at the top.
    pub fn next(self) -> Option<Self> {
        match self {
            Level::ErrorContext => Some(Level::BuildTree),
            Level::BuildTree => Some(Level::BuildRoot),
            Level::BuildRoot => Some(Level::FullLog),
            Level::FullLog => None,
        }
    }

    /// Whether this rung reads the filesystem rather than the log.
    pub fn touches_filesystem(self) -> bool {
        matches!(self, Level::BuildTree | Level::BuildRoot)
    }

    /// Whether reaching this rung requires a build to have failed already.
    ///
    /// `BUILDROOT` is populated by `%install`; before a failure there is
    /// nothing there worth looking at.
    pub fn requires_failure(self) -> bool {
        matches!(self, Level::BuildRoot)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Level::ErrorContext => "error-context",
            Level::BuildTree => "build-tree",
            Level::BuildRoot => "buildroot",
            Level::FullLog => "full-log",
        };
        f.write_str(s)
    }
}

/// Why a rung was climbed. Recorded so metrics can show how often each rung was
/// actually necessary — if `FullLog` never helps, delete it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationReason {
    /// The model asked for more, explicitly.
    ModelRequested,
    /// The current rung produced nothing usable — an empty excerpt, no frames.
    NothingUsable,
    /// The same error signature came back after a fix attempt, so whatever the
    /// model was looking at did not contain the answer. The only reason that
    /// justifies climbing rather than resetting.
    RepeatedSignature,
}

/// Why a rung was passed over on the way up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// The rung needs a failed build and there has not been one.
    NoFailureYet,
}

/// Refusal to climb any further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denied {
    /// Nothing left above the current rung.
    AtMaximum,
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Denied::AtMaximum => f.write_str("already at the highest context level"),
        }
    }
}

/// How far up the ladder one error signature has driven the context.
///
/// Discard it and build a new one when the signature changes; the next error
/// gets the cheap excerpt again, however high the previous one climbed.
#[derive(Debug, Clone)]
pub struct Escalation {
    level: Level,
    build_failed: bool,
    history: Vec<(Level, EscalationReason)>,
    skipped: Vec<(Level, Skipped)>,
}

impl Escalation {
    /// Start at the bottom rung.
    ///
    /// `build_failed` gates [`Level::BuildRoot`]; pass what actually happened,
    /// not what is expected to happen.
    pub fn new(build_failed: bool) -> Self {
        Self {
            level: Level::ErrorContext,
            build_failed,
            history: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn history(&self) -> &[(Level, EscalationReason)] {
        &self.history
    }

    /// Rungs passed over, and why.
    pub fn skipped(&self) -> &[(Level, Skipped)] {
        &self.skipped
    }

    /// Climb to the next *available* rung.
    ///
    /// Unavailable rungs are skipped rather than refused. This matters now that
    /// `BuildRoot` sits below `FullLog`: treating "no failure yet" as a hard
    /// denial would dead-end the ladder and make the last rung unreachable for
    /// exactly the runs most likely to need it.
    pub fn escalate(&mut self, reason: EscalationReason) -> Result<Level, Denied> {
        let mut candidate = self.level;

        let next = loop {
            candidate = candidate.next().ok_or(Denied::AtMaximum)?;

            if candidate.requires_failure() && !self.build_failed {
                self.skipped.push((candidate, Skipped::NoFailureYet));
                continue;
            }

            break candidate;
        };

        self.level = next;
        self.history.push((next, reason));
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_cheapest_rung() {
        let e = Escalation::new(false);
        assert_eq!(e.level(), Level::ErrorContext);
        assert!(!e.level().touches_filesystem());
    }

    #[test]
    fn full_log_is_last() {
        let mut e = Escalation::new(true);
        assert_eq!(
            e.escalate(EscalationReason::NothingUsable),
            Ok(Level::BuildTree)
        );
        assert_eq!(
            e.escalate(EscalationReason::NothingUsable),
            Ok(Level::BuildRoot)
        );
        assert_eq!(
            e.escalate(EscalationReason::NothingUsable),
            Ok(Level::FullLog)
        );
        assert_eq!(
            e.escalate(EscalationReason::NothingUsable),
            Err(Denied::AtMaximum)
        );
    }

    #[test]
    fn files_are_read_before_the_whole_log() {
        // The ordering change: cheap targeted reads come first, and the 35k
        // token log is only reached after they have been tried.
        let mut e = Escalation::new(true);
        e.escalate(EscalationReason::ModelRequested).unwrap();
        assert!(e.level().touches_filesystem());
        assert_ne!(e.level(), Level::FullLog);
    }

    #[test]
    fn buildroot_is_skipped_not_fatal_without_a_failure() {
        // The regression the reorder introduced: a hard denial here would make
        // FullLog unreachable.
        let mut e = Escalation::new(false);
        assert_eq!(
            e.escalate(EscalationReason::ModelRequested),
            Ok(Level::BuildTree)
        );
        assert_eq!(
            e.escalate(EscalationReason::ModelRequested),
            Ok(Level::FullLog),
            "must step over BuildRoot, not stop at it"
        );
        assert_eq!(e.skipped(), &[(Level::BuildRoot, Skipped::NoFailureYet)]);
    }

    #[test]
    fn history_records_why() {
        let mut e = Escalation::new(true);
        e.escalate(EscalationReason::RepeatedSignature).unwrap();
        assert_eq!(
            e.history(),
            &[(Level::BuildTree, EscalationReason::RepeatedSignature)]
        );
    }

    #[test]
    fn a_new_error_signature_starts_over_at_the_bottom() {
        // The loop is iterative: height reached on one error is not carried
        // into the next. Whatever the previous signature cost, a new one is
        // a new `Escalation` and gets the cheap excerpt again.
        let mut previous = Escalation::new(true);
        previous
            .escalate(EscalationReason::RepeatedSignature)
            .unwrap();
        previous
            .escalate(EscalationReason::RepeatedSignature)
            .unwrap();
        assert_eq!(previous.level(), Level::BuildRoot);

        let next = Escalation::new(true);
        assert_eq!(next.level(), Level::ErrorContext);
        assert!(next.history().is_empty());
    }

    #[test]
    fn declaration_order_is_the_ladder_order() {
        // `next()` and the derived `Ord` have to agree, or a caller comparing
        // levels sees a different ladder than one climbing it.
        let rungs = [
            Level::ErrorContext,
            Level::BuildTree,
            Level::BuildRoot,
            Level::FullLog,
        ];
        assert!(rungs.is_sorted(), "enum order must be the ladder order");
        for pair in rungs.windows(2) {
            assert_eq!(pair[0].next(), Some(pair[1]));
        }
        assert_eq!(rungs.last().unwrap().next(), None);
    }
}
