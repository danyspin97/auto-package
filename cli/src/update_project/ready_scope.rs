//! What the ready gate of a submit reads to tell whether the package carries
//! unpublished work. `lint --submit` judges only those packages, as
//! `check_built` judges a built one: a package with nothing to review gets no
//! review (and no download) and no verdict.

use super::existing_lint;
use super::leftovers;
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
