//! What the ready gate of a submit reads to tell whether the package carries
//! unpublished work. `lint --submit` judges only those packages, as
//! `check_built` judges a built one: a package with nothing to review gets no
//! review (and no download) and no verdict.

use super::existing_lint;
use super::leftovers;
use super::ready::{Judging, ReadyCheck};
use super::*;

/// The inputs of `leftovers::carry` for a submit of this run.
#[derive(Clone)]
pub(crate) struct CarryScope {
    source: String,
    in_place: bool,
    continues: bool,
}

impl CarryScope {
    pub(super) fn of(a: &UpdateProjectArgs) -> Self {
        CarryScope {
            source: a.source.clone(),
            in_place: a.in_place,
            continues: existing_lint::applies(a),
        }
    }

    /// Whether the branch `branch` of `item` carries unpublished work.
    pub(super) fn carries(&self, branch: &str, item: &Item, emit: Emit<'_>) -> bool {
        leftovers::carry(
            &self.source,
            branch,
            &item.pkg,
            self.in_place,
            self.continues,
            Some(&item.ecosystem),
            emit,
        )
        .is_some()
    }
}

/// The check of a `fix --resubmit`: its request is always judged, with the
/// defaults of `lint` (an agent runs, an inherited build failure does not
/// block). `fix` has no flag for either.
pub(crate) fn for_resubmit() -> ReadyCheck {
    ReadyCheck::Gate {
        judging: Judging {
            agent: true,
            block_inherited: false,
        },
        carry: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fix_resubmit_is_judged_with_the_lint_defaults() {
        match for_resubmit() {
            ReadyCheck::Gate { judging, carry } => {
                assert_eq!(
                    judging,
                    Judging {
                        agent: true,
                        block_inherited: false
                    }
                );
                assert!(carry.is_none());
            }
            ReadyCheck::Skip => panic!("a resubmit is judged by the ready gate"),
        }
    }
}
