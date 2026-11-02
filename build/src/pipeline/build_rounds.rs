//! The round limits of the fix-after-build loop: each failed build that gets a
//! fix turn is one round. A package ends when it has used its rounds, or when
//! the same build errors came back in a row with no progress between them.

use std::collections::BTreeSet;

/// The rounds one package has been given a fix turn for, and the error lines
/// of its last failed build.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct BuildRounds {
    /// Fix turns given so far. Restored from `PkgState::cumulative_build_rounds`
    /// when the package comes back in the same CLI run.
    pub(crate) given: usize,
    /// The error lines of the last failed build, sorted and joined; `None`
    /// when that build named no error line.
    last: Option<String>,
    /// Failed builds in a row whose error lines are `last`.
    repeats: usize,
}

impl BuildRounds {
    /// Record one failed build (its `error_lines` from the log extractor) and
    /// decide whether it gets a fix turn. `Err` is the reason the package ends
    /// with, and no turn is paid for it.
    pub(crate) fn next_round(&mut self, error_lines: &[String]) -> Result<(), String> {
        self.next_round_with(
            error_lines,
            ap_core::limits::DEFAULT_FIX_BUILD_ROUNDS,
            ap_core::limits::SAME_BUILD_ERRORS_LIMIT,
        )
    }

    /// [`next_round`](Self::next_round) with the limits given, so the loop can
    /// be run with any cap.
    pub(crate) fn next_round_with(
        &mut self,
        error_lines: &[String],
        cap: usize,
        same_limit: usize,
    ) -> Result<(), String> {
        let key = fingerprint(error_lines);
        if key.is_some() && key == self.last {
            self.repeats += 1;
        } else {
            self.repeats = 1;
            self.last = key.clone();
        }
        let first = error_lines
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        if key.is_some() && self.repeats >= same_limit {
            return Err(format!(
                "the same build errors came back {} times: {first}",
                self.repeats
            ));
        }
        if self.given >= cap {
            return Err(format!("fix-after-build round limit {cap} reached"));
        }
        self.given += 1;
        Ok(())
    }
}

/// The error lines as one key, independent of their order and of repeats.
/// `None` when there is no error line at all.
fn fingerprint(error_lines: &[String]) -> Option<String> {
    let set: BTreeSet<&str> = error_lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    (!set.is_empty()).then(|| set.into_iter().collect::<Vec<_>>().join("\n"))
}
