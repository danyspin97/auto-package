//! What a package goes through once its branch and reads are done, under
//! `--update-deps`.
//!
//! A package whose BuildRequires need an update of another package of its
//! source project waits for the sweep to find it (see [`super::same_project`]
//! and [`super::same_project_defer`]). A package that waits is parked, with
//! what was read for it, and holds no build slot. It is judged again each
//! time the run queues a package, and released once it no longer waits, after
//! the packages it waits for. Whatever is still parked when the run stops is
//! listed as not started.

use super::parked::{Parked, release_first};
use super::same_project::{
    FoundIndex, NeedSameProject, needs_prediction, same_project_after, same_project_need,
    same_project_providers,
};
use super::same_project_defer::{Deferral, MAX_DEFERRED, WaitGraph, defer_decision};
use super::throttle::Throttle;
use super::*;
use ap_build::spec::Requirement;
use ap_build::update_project::RequiredUpdate;
use std::cell::OnceCell;

/// What `--update-deps` knows of the run's same-project waits.
#[derive(Default)]
pub(super) struct SameWait {
    /// Whether the run waits for same-project updates at all: with a sweep,
    /// `--update-deps` and without `--test-against-updates`.
    active: bool,
    /// The updates the sweep found so far.
    pub(super) index: FoundIndex,
    /// The names (see `visit_key`) of every job pushed to `ready`: only these
    /// are built, so only these may be waited for.
    pub(super) queued: HashSet<String>,
    /// The parked packages and the packages each waits for.
    pub(super) graph: WaitGraph,
    pub(super) parked: Parked<Held>,
}

impl SameWait {
    pub(super) fn new(active: bool) -> Self {
        SameWait {
            active,
            ..Default::default()
        }
    }

    /// An update the sweep found (recorded only when the run is active).
    pub(super) fn found(&mut self, item: &Item) {
        if self.active {
            self.index.insert(item);
        }
    }

    /// The sweep has ended. Nothing waits for it any more, so the parked
    /// packages are released: returns whether there are any.
    pub(super) fn sweep_ended(&mut self) -> bool {
        if self.active {
            self.index.finish();
        }
        self.has_parked()
    }

    /// Push `job` to `ready`, and note its names as queued.
    pub(super) fn push_ready(&mut self, ready: &mut Vec<Job>, job: Job) {
        self.queued.extend(job.keys.iter().cloned());
        ready.push(job);
    }

    pub(super) fn has_parked(&self) -> bool {
        !self.parked.is_empty()
    }

    /// Put `held` aside until each of `on` is found or released.
    pub(super) fn park(&mut self, held: Held, on: &[String]) {
        let pkg = held.item.pkg.clone();
        self.graph.defer(&pkg, on);
        self.parked.park(&pkg, held);
    }

    /// The first parked package that no longer waits, released from the
    /// park with what its judgment says. Packages still waiting are judged
    /// too, so their waits are kept current.
    pub(super) fn release_next(
        &mut self,
        source: &str,
        branch: &str,
        ctx: &Ctx<'_>,
    ) -> Option<Decided> {
        if !self.active {
            return None;
        }
        let order = self.graph.release_order();
        let SameWait {
            index,
            graph,
            parked,
            ..
        } = self;
        let (key, held, (deferral, needs)) =
            release_first(parked, &order, |key, held| {
                match judge(&*index, &*graph, held, source, branch, ctx) {
                    (Deferral::Defer { on, .. }, _) => {
                        graph.defer(key, &on);
                        None
                    }
                    judged => Some(judged),
                }
            })?;
        graph.release(&key);
        Some(Decided {
            held,
            deferral,
            needs,
        })
    }

    /// The parked packages' items, in the order they were parked: what a
    /// run that stops before releasing them lists as not started.
    pub(super) fn into_parked_items(self) -> Vec<Item> {
        self.parked
            .into_values()
            .into_iter()
            .map(|held| held.item)
            .collect()
    }
}

/// A package judged for its wait: `deferral` is not `Defer`, so it goes on,
/// with what it `needs` of its source's packages.
pub(super) struct Decided {
    pub(super) held: Held,
    pub(super) deferral: Deferral,
    pub(super) needs: Vec<NeedSameProject>,
}

/// Judge `held` against the sweep's updates so far and the parked packages.
/// The prediction is read here, once, when it can change the judgment (see
/// [`needs_prediction`]).
pub(super) fn judge(
    index: &FoundIndex,
    graph: &WaitGraph,
    held: &mut Held,
    source: &str,
    branch: &str,
    ctx: &Ctx<'_>,
) -> (Deferral, Vec<NeedSameProject>) {
    let providers = same_project_providers(&held.verdicts, source, branch, index);
    let needs = if providers.is_empty() {
        Vec::new()
    } else {
        let unread = depreach::Prediction::Unknown;
        let prediction = if needs_prediction(&held.item.pkg, &held.reqs, &providers, index) {
            held.prediction
                .get_or_insert_with(|| predict(&held.item, ctx))
        } else {
            &unread
        };
        same_project_need(&held.item.pkg, &held.reqs, prediction, &providers, index)
    };
    let deferral = defer_decision(
        &held.item.pkg,
        &needs,
        graph,
        MAX_DEFERRED,
        index.sweep_finished(),
        held.item.dependency_of.as_deref(),
    );
    (deferral, needs)
}

/// A package whose branch is made and whose BuildRequires were read: what
/// its dependencies are chosen from, kept so nothing is read again.
pub(super) struct Held {
    pub(super) item: Item,
    /// The name it was found under (see `visit_key`).
    pub(super) first_key: String,
    pub(super) reqs: Vec<Requirement>,
    pub(super) verdicts: Vec<depreach::DepVerdict>,
    pub(super) adds: Vec<PathAddition>,
    /// What its new release requires, once read (see `deps::predict`).
    pub(super) prediction: Option<depreach::Prediction>,
}

impl Held {
    pub(super) fn new(
        item: Item,
        first_key: String,
        reqs: Vec<Requirement>,
        verdicts: Vec<depreach::DepVerdict>,
        adds: Vec<PathAddition>,
    ) -> Self {
        Held {
            item,
            first_key,
            reqs,
            verdicts,
            adds,
            prediction: None,
        }
    }
}

/// Settle a package whose branch is made and whose BuildRequires are read:
/// it is parked when it waits for a same-project update (its build slot is
/// given back), and otherwise goes on (see `go_on`).
#[allow(clippy::too_many_arguments)]
pub(super) fn settle(
    mut held: Held,
    a: &UpdateProjectArgs,
    branch: &str,
    ctx: &Ctx<'_>,
    throttle: &Throttle,
    same: &mut SameWait,
    queue: &mut VecDeque<Item>,
    visited: &mut HashSet<String>,
    dependency_count: &mut usize,
    ready: &mut Vec<Job>,
) {
    let (deferral, needs) = if same.active {
        judge(&same.index, &same.graph, &mut held, &a.source, branch, ctx)
    } else {
        (Deferral::Proceed(None), Vec::new())
    };
    if let Deferral::Defer { on, note } = &deferral {
        // Branching queued a build, so the slot it took is given back: a
        // package that waits holds none.
        if !a.dry_run {
            throttle.dropped();
        }
        ctx.say(format!("{}: {note}", held.item.pkg));
        same.park(held, on);
        return;
    }
    go_on(
        Decided {
            held,
            deferral,
            needs,
        },
        a,
        branch,
        ctx,
        queue,
        visited,
        dependency_count,
        same,
        ready,
    );
}

/// Let a judged package go on to its build: what it waits for is queued
/// ahead of it, the same-project updates it needs are requested, and the
/// dependencies `--update-deps` chooses for it are queued ahead of it too.
#[allow(clippy::too_many_arguments)]
pub(super) fn go_on(
    decided: Decided,
    a: &UpdateProjectArgs,
    branch: &str,
    ctx: &Ctx<'_>,
    queue: &mut VecDeque<Item>,
    visited: &mut HashSet<String>,
    dependency_count: &mut usize,
    same: &mut SameWait,
    ready: &mut Vec<Job>,
) {
    let Decided {
        mut held,
        deferral,
        needs,
    } = decided;
    match &deferral {
        Deferral::Proceed(Some(why)) | Deferral::BuildAgainstOrigin(why) => ctx.say(why.clone()),
        _ => {}
    }
    let mut after: Vec<String> = Vec::new();
    let order = same_project_after(&held.item.pkg, &needs, &same.index, &same.queued);
    after.extend(order.waits.iter().map(|w| w.key.clone()));
    for w in &order.waits {
        if let Some(requirement) = &w.required {
            held.item.requires.push(RequiredUpdate {
                pkg: w.dep.clone(),
                branch_package: format!("{branch}/{}", w.dep),
                requirement: requirement.clone(),
                origin: a.source.clone(),
                origin_version: w.origin_version.clone(),
                request: None,
            });
        }
    }
    for note in &order.notes {
        ctx.say(note.clone());
    }
    if a.update_deps {
        // The new release's requirements are read only when a dependency
        // from another project needs choosing for.
        let prediction = OnceCell::new();
        if let Some(p) = held.prediction.take() {
            let _ = prediction.set(p);
        }
        let predict_once = || prediction.get_or_init(|| predict(&held.item, ctx)).clone();
        let found = dependency_items(
            a,
            branch,
            &held.item,
            &held.reqs,
            &held.verdicts,
            &held.adds,
            &predict_once,
            ctx,
            Emit::run(ctx.reporter.as_ref()),
        );
        let mut deps = DepQueue {
            queue,
            visited,
            count: dependency_count,
        };
        for (dep, choice) in found {
            deps.add(
                &mut held.item,
                dep,
                choice,
                &mut after,
                ctx.reporter.as_ref(),
            );
        }
    }
    same.push_ready(ready, Job::new(held.item, held.first_key, after));
}
