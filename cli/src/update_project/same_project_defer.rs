//! Whether a package waits for the sweep to find the update of another
//! package of its source project that it needs (see [`super::same_project`]).
//!
//! The sweep finds updates in package order, so `A` may be prepared before
//! the sweep reaches the `B` it needs. `A` is then deferred: put aside, not
//! handed over, while the preparation goes on with what the sweep finds
//! next. Deferring is bounded: never beyond a cap of deferred packages,
//! never into a cycle, and never past the end of the sweep.
//!
//! # Wiring contract (for `cmd.rs`'s preparation loop)
//!
//! - Keep one [`WaitGraph`] for the run, beside the deferred items
//!   themselves (the `Item`, its first key, and what was read for it:
//!   requirements, prediction, verdicts, path additions, so nothing is read
//!   again on release).
//! - After [`super::same_project::same_project_need`], call
//!   [`defer_decision`] with the index's `sweep_finished()`:
//!   - [`Deferral::Defer`]: call [`WaitGraph::defer`] with `on`, park the
//!     item, and call `throttle.dropped()` for it (and for each dependency
//!     of another project already let through for it, which stay parked
//!     with it, not queued). A deferred package holds no throttle slot, so
//!     however long the list grows the builds of the others go on. Show
//!     its `note` (`waiting for B`) as the package's TUI row
//!     state and say it as progress. Then go on with the loop.
//!   - [`Deferral::Proceed`] / [`Deferral::BuildAgainstOrigin`]: say the
//!     reason, if any, and go on as today with
//!     [`super::same_project::same_project_after`].
//! - Each time the loop pushes a job to `ready`, and once more when
//!   `incoming()` returns `None` (after `FoundIndex::finish`), go over
//!   [`WaitGraph::release_order`]: judge each deferred package again with
//!   the index as it is now, and for each that no longer gets `Defer`, call
//!   [`WaitGraph::release`], `throttle.admit()` for it (stop as today when
//!   that says the run is stopping), then continue its preparation where it
//!   left off. At the end of the sweep nothing gets `Defer`, so every
//!   deferred package is released, each after those it waits for.
//! - A dependency of another package (`Item::dependency_of`) is never
//!   deferred: it goes on, and the package that needs it waits for it.
//! - A package that ends deferred because the run stops is listed as
//!   `NotStarted`, as the items dropped on a stop are.
//! - The summary line for a package that waited: `waited for B`; for one
//!   released by a cycle or the cap, the [`Deferral::BuildAgainstOrigin`]
//!   reason.

use super::same_project::{NeedSameProject, SameNeed};
use std::collections::BTreeMap;

/// How many packages may be deferred at once, at most.
pub(super) const MAX_DEFERRED: usize = 25;

/// The deferred packages and the packages each waits for, all by
/// [`ap_build::osc::fuzzy_key`] of the package name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WaitGraph {
    waits: BTreeMap<String, Vec<String>>,
}

impl WaitGraph {
    /// How many packages are deferred.
    pub(super) fn len(&self) -> usize {
        self.waits.len()
    }

    pub(super) fn is_deferred(&self, pkg: &str) -> bool {
        self.waits.contains_key(&key(pkg))
    }

    /// Defer `pkg` until each of `on` is found or released, replacing what
    /// it waited for before.
    pub(super) fn defer(&mut self, pkg: &str, on: &[String]) {
        self.waits
            .insert(key(pkg), on.iter().map(|p| key(p)).collect());
    }

    /// `pkg` is no longer deferred.
    pub(super) fn release(&mut self, pkg: &str) {
        self.waits.remove(&key(pkg));
    }

    /// Whether `from` waits for `to`, directly or through other deferred
    /// packages.
    fn reaches(&self, from: &str, to: &str) -> bool {
        let (from, to) = (key(from), key(to));
        let mut seen: Vec<&str> = Vec::new();
        let mut stack: Vec<&str> = vec![from.as_str()];
        while let Some(at) = stack.pop() {
            if at == to {
                return true;
            }
            if seen.contains(&at) {
                continue;
            }
            seen.push(at);
            if let Some(next) = self.waits.get(at) {
                stack.extend(next.iter().map(String::as_str));
            }
        }
        false
    }

    /// The deferred packages (their keys), each after the deferred packages
    /// it waits for, otherwise in key order; a cycle is broken at the first
    /// key left.
    pub(super) fn release_order(&self) -> Vec<String> {
        let mut left: Vec<&String> = self.waits.keys().collect();
        let mut order = Vec::with_capacity(left.len());
        while !left.is_empty() {
            let free = left
                .iter()
                .position(|k| self.waits[*k].iter().all(|w| w == *k || !left.contains(&w)));
            order.push(left.remove(free.unwrap_or(0)).clone());
        }
        order
    }
}

fn key(pkg: &str) -> String {
    ap_build::osc::fuzzy_key(pkg)
}

/// What becomes of a package that may wait for same-project updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Deferral {
    /// Put the package aside until each of `on` (package names) is found by
    /// the sweep or released itself. `note` is the row's state:
    /// `waiting for B, C`.
    Defer { on: Vec<String>, note: String },
    /// Nothing to wait for (any more). The reason, when there is one to say:
    /// what ended the wait.
    Proceed(Option<String>),
    /// A dependency it needs is not waited for, though it might bring the
    /// update: waiting would close a cycle, or too many packages wait
    /// already. The package goes on as today; the reason says which.
    BuildAgainstOrigin(String),
}

/// Whether `a`, with `needs` of its source's packages, is deferred.
/// `dependency_of` is the package `a` is a dependency of, when it is one: a
/// dependency is never deferred, since the package that needs it is queued
/// behind it (see `DepQueue::add`), and parking it would leave that package
/// with a dependency that is never handed over.
///
/// `a` waits for each `B` it needs that the sweep has not found yet
/// ([`SameNeed::NotFoundYet`]), and for each found `B` it needs
/// ([`SameNeed::Needs`], [`SameNeed::Unknown`]) that is deferred itself,
/// since `B` is not queued until it is released (the chain `A -> B -> C`).
///
/// Not deferred: when the sweep has finished (every wait ends there), for a
/// `B` that waits for `a` (the cycle `A <-> B`: `a` goes on as today), and
/// when `a` is not deferred yet and `cap` packages are. A package already
/// deferred is judged again without the cap, so it is never pushed out.
pub(super) fn defer_decision(
    a: &str,
    needs: &[NeedSameProject],
    graph: &WaitGraph,
    cap: usize,
    sweep_finished: bool,
    dependency_of: Option<&str>,
) -> Deferral {
    let blockers: Vec<&str> = needs
        .iter()
        .filter(|n| match &n.need {
            SameNeed::NotFoundYet { .. } => true,
            SameNeed::Needs { .. } | SameNeed::Unknown { .. } => graph.is_deferred(&n.dep),
            _ => false,
        })
        .map(|n| n.dep.as_str())
        .collect();
    let was_waiting = graph.is_deferred(a);
    if blockers.is_empty() {
        return Deferral::Proceed(was_waiting.then(|| format!("{a}: no longer waiting")));
    }
    if let Some(parent) = dependency_of {
        return Deferral::BuildAgainstOrigin(format!(
            "{a} is needed by {parent}, which waits for it: not waiting for {}, {a} builds \
             against the source's",
            blockers.join(", ")
        ));
    }
    if sweep_finished {
        return Deferral::Proceed(Some(format!(
            "{a}: the sweep has finished, no longer waiting for {}",
            blockers.join(", ")
        )));
    }
    let (cyclic, on): (Vec<&str>, Vec<&str>) =
        blockers.into_iter().partition(|b| graph.reaches(b, a));
    if on.is_empty() {
        let names = cyclic.join(", ");
        return Deferral::BuildAgainstOrigin(format!(
            "{a} needs the update of {names}, which waits for {a}: not waiting, {a} builds \
             against {names} as the source has it"
        ));
    }
    if !was_waiting && graph.len() >= cap {
        return Deferral::BuildAgainstOrigin(format!(
            "{a} needs the update of {}, which the sweep has not found yet, but {} packages \
             wait already: not waiting, {a} builds against the source's",
            on.join(", "),
            graph.len()
        ));
    }
    let on: Vec<String> = on.into_iter().map(str::to_string).collect();
    Deferral::Defer {
        note: format!("waiting for {}", on.join(", ")),
        on,
    }
}
