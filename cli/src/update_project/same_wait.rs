//! What a package goes through once its branch and reads are done, under
//! `--update-deps`: the dependencies chosen for it are queued ahead of it,
//! and it is handed over to the builds with what it waits for.

use super::*;
use ap_build::spec::Requirement;
use std::cell::OnceCell;

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

/// Let `held` go on to its build: the dependencies `--update-deps` chooses
/// for it are queued ahead of it, and it is pushed to `ready`.
#[allow(clippy::too_many_arguments)]
pub(super) fn go_on(
    mut held: Held,
    a: &UpdateProjectArgs,
    branch: &str,
    ctx: &Ctx<'_>,
    queue: &mut VecDeque<Item>,
    visited: &mut HashSet<String>,
    dependency_count: &mut usize,
    ready: &mut Vec<Job>,
) {
    let mut after: Vec<String> = Vec::new();
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
    ready.push(Job::new(held.item, held.first_key, after));
}
