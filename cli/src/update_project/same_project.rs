//! What a package needs of the updates the same run finds for other packages
//! of its own source project, under `--update-deps`.
//!
//! [`depreach::dependency_origins`] never counts the source project as an
//! origin, so a package `A` whose BuildRequires need the new version of `B`,
//! another package of the source, is built against `B` as the source has it
//! even when the sweep found an update of `B`. This module decides, without
//! any I/O, what `A` needs of such a `B`; `same_project_defer`
//! decides whether `A` waits for the sweep to find it.
//!
//! # Wiring contract (for `cmd.rs`'s preparation loop)
//!
//! The preparation loop makes these calls through `same_wait`.
//!
//! 1. Keep one [`FoundIndex`] for the run. Call [`FoundIndex::insert`] for
//!    every item `incoming()` returns from the sweep (before the checks that
//!    may skip it), and [`FoundIndex::finish`] once `incoming()` returns
//!    `None`. Lint and backport runs have no sweep: do not use this there.
//! 2. Keep a `queued: HashSet<String>` with every key of every `Job` pushed
//!    to `ready` (its `Job::keys`), and only those: a skipped or failed item
//!    is never built, so nothing may wait for it or name its branch.
//! 3. After `reach_dependencies` gave `verdicts` for `A`, and only with
//!    `--update-deps`, build the providers with [`same_project_providers`]
//!    (`a.source`, the branch project, the index) and judge them with
//!    [`same_project_need`], with the same requirements and prediction
//!    `dependency_items` gets.
//! 4. Ask `same_project_defer::defer_decision` (see its module for
//!    deferring, the cap, the throttle slot and the release at sweep end).
//! 5. When `A` goes on, call [`same_project_after`] with `queued`: extend
//!    `after` with each [`WaitFor::key`], push each
//!    [`WaitFor::required`] onto `item.requires` (a `RequiredUpdate` with
//!    `branch_package` `{branch}/{dep}`, `origin` the source and
//!    `origin_version` [`WaitFor::origin_version`]), and say each
//!    [`SameOrder::notes`] line as progress.
//!
//! `--test-against-updates` is not looked at here: it only widens which
//! dependencies of other projects are branched, and a same-project `B` the
//! sweep found is branched anyway.

use super::*;
use ap_build::depreach::{DepNeed, Prediction};
use ap_build::spec::Requirement;
use std::collections::HashMap;

/// An update the sweep found, as [`FoundIndex`] keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    /// The package's name in the source, as OBS lists it.
    pub(super) pkg: String,
    pub(super) old_version: String,
    pub(super) new_version: String,
    /// The key its job is known by (see `visit_key`), which `after` names.
    pub(super) key: String,
}

/// The updates the run's sweep found so far, by [`osc::fuzzy_key`] of the
/// package name, and whether the sweep has finished.
#[derive(Debug, Clone, Default)]
pub(super) struct FoundIndex {
    found: HashMap<String, Found>,
    finished: bool,
}

impl FoundIndex {
    /// Record `item`, an update the sweep found. A package found twice keeps
    /// what was found first, as the preparation does (`visited`).
    pub(super) fn insert(&mut self, item: &Item) {
        self.found
            .entry(osc::fuzzy_key(&item.pkg))
            .or_insert_with(|| Found {
                pkg: item.pkg.clone(),
                old_version: item.old_version.clone(),
                new_version: item.new_version.clone(),
                key: visit_key(item),
            });
    }

    /// The sweep has found everything it will: a package not in the index
    /// now has no update in this run.
    pub(super) fn finish(&mut self) {
        self.finished = true;
    }

    pub(super) fn sweep_finished(&self) -> bool {
        self.finished
    }

    /// The update found for the package named `pkg` (any spelling
    /// [`osc::fuzzy_key`] folds together).
    pub(super) fn get(&self, pkg: &str) -> Option<&Found> {
        self.found.get(&osc::fuzzy_key(pkg))
    }
}

/// A package of the source project that a package's BuildRequires name, by
/// the names it requires it by: what [`depreach::DependencyOrigin`] is for a
/// dependency of another project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SameProjectProvider {
    /// The source package, in the source project.
    pub(super) package: String,
    /// The package names and capabilities (with their bound) it is required
    /// by, as [`depreach::DepVerdict::name`] has them.
    pub(super) names: Vec<String>,
    /// For a capability in `names`, the version its provider's `Provides:`
    /// gives it.
    pub(super) provided: Vec<(String, String)>,
    /// The version of the binary the build would use, when the search gave
    /// one: what a package name is compared with while the sweep has not
    /// found the package (once found, its old version is used).
    pub(super) origin_version: Option<String>,
}

/// The packages of `source` the `verdicts` resolve to, merged by package.
/// A hit in `branch` counts only for a package `index` has: the branch also
/// holds the dependencies of other projects this run branched.
///
/// A capability no provider meets counts by each provider found in those
/// projects, since the update its bound needs may come from that package.
pub(super) fn same_project_providers(
    verdicts: &[depreach::DepVerdict],
    source: &str,
    branch: &str,
    index: &FoundIndex,
) -> Vec<SameProjectProvider> {
    use depreach::{CapState, DepState};
    let mut out: Vec<SameProjectProvider> = Vec::new();
    let mut push = |project: &str,
                    package: Option<&String>,
                    name: &str,
                    provide: Option<&str>,
                    version: Option<&str>| {
        let Some(package) = package else { return };
        let ours = project == source || (project == branch && index.get(package).is_some());
        if !ours {
            return;
        }
        let key = osc::fuzzy_key(package);
        let at = out.iter().position(|p| osc::fuzzy_key(&p.package) == key);
        let p = match at {
            Some(i) => &mut out[i],
            None => {
                out.push(SameProjectProvider {
                    package: package.clone(),
                    names: Vec::new(),
                    provided: Vec::new(),
                    origin_version: None,
                });
                out.last_mut().expect("just pushed")
            }
        };
        if !p.names.iter().any(|n| n == name) {
            p.names.push(name.to_string());
        }
        if let Some(v) = provide
            && !p.provided.iter().any(|(n, _)| n == name)
        {
            p.provided.push((name.to_string(), v.to_string()));
        }
        if p.origin_version.is_none() {
            p.origin_version = version.map(str::to_string);
        }
    };
    for v in verdicts {
        match &v.state {
            DepState::Reachable { provider: h } => push(
                &h.project,
                h.package.as_ref(),
                &v.name,
                None,
                h.version.as_deref(),
            ),
            DepState::Capability(CapState::Provided { provider: h }) => push(
                &h.project,
                h.package.as_ref(),
                &v.name,
                h.version.as_deref(),
                None,
            ),
            DepState::Capability(CapState::NoProvider { found, .. }) => {
                for f in found {
                    push(
                        &f.project,
                        f.package.as_ref(),
                        &v.name,
                        Some(&f.version),
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    out
}

/// What a package needs of one package of its own source project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NeedSameProject {
    /// The source package depended on.
    pub(super) dep: String,
    pub(super) need: SameNeed,
}

/// `bound` is always the requirement as the spec writes it (`libb-devel >=
/// 2.0`), followed by where it comes from when PyPI's metadata gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SameNeed {
    /// The found update meets a bound the source's version misses: the
    /// dependency is built first and the package's request names it.
    Needs { new_version: String, bound: String },
    /// The source's version meets every bound, or none is stated.
    NotNeeded,
    /// The found update misses the bound too: the package goes on as today.
    Unmet { new_version: String, bound: String },
    /// The source's version misses the bound or cannot be compared with it,
    /// and the sweep, still running, has not found an update of the
    /// dependency yet: the package may wait for it.
    NotFoundYet { bound: String },
    /// As `NotFoundYet`, but the sweep finished without an update of it.
    NoUpdate { bound: String },
    /// A bound that cannot be compared with the found versions: the
    /// dependency is built first, as `DepChoice::UpdateUnchecked` does, but
    /// the request does not name it.
    Unknown { new_version: String, bound: String },
}

/// What `parent` needs of each of `providers` (other than itself), given its
/// requirements `reqs`, what its new release is predicted to require, and
/// the updates found so far. Judged by [`depreach::dependency_need`] and
/// [`depreach::predicted_need`] as `choose_dependency` judges a dependency of
/// another project, the stronger of the two deciding.
///
/// A package the sweep has not found (yet) is judged against its own
/// version: the provider's `origin_version` for a package name, `provided`
/// for a capability.
pub(super) fn same_project_need(
    parent: &str,
    reqs: &[Requirement],
    prediction: &Prediction,
    providers: &[SameProjectProvider],
    index: &FoundIndex,
) -> Vec<NeedSameProject> {
    let own = osc::fuzzy_key(parent);
    providers
        .iter()
        .filter(|p| osc::fuzzy_key(&p.package) != own)
        .map(|p| {
            let need = match index.get(&p.package) {
                Some(f) => {
                    let (new_version, old) = (f.new_version.clone(), f.old_version.as_str());
                    match judge(parent, reqs, prediction, p, old, &new_version) {
                        DepNeed::Update(bound) => SameNeed::Needs { new_version, bound },
                        DepNeed::Origin => SameNeed::NotNeeded,
                        DepNeed::Unmet(bound) => SameNeed::Unmet { new_version, bound },
                        DepNeed::Unknown(bound) => SameNeed::Unknown { new_version, bound },
                    }
                }
                None => {
                    let old = p.origin_version.as_deref().unwrap_or("");
                    match judge(parent, reqs, prediction, p, old, old) {
                        DepNeed::Origin | DepNeed::Update(_) => SameNeed::NotNeeded,
                        DepNeed::Unmet(bound) | DepNeed::Unknown(bound)
                            if index.sweep_finished() =>
                        {
                            SameNeed::NoUpdate { bound }
                        }
                        DepNeed::Unmet(bound) | DepNeed::Unknown(bound) => {
                            SameNeed::NotFoundYet { bound }
                        }
                    }
                }
            };
            NeedSameProject {
                dep: p.package.clone(),
                need,
            }
        })
        .collect()
}

/// The stronger of what the spec and the prediction need of `p` between
/// `old` and `new`, the requirement named with where it comes from.
fn judge(
    parent: &str,
    reqs: &[Requirement],
    prediction: &Prediction,
    p: &SameProjectProvider,
    old: &str,
    new: &str,
) -> DepNeed {
    let spec = depreach::dependency_need(reqs, &p.names, old, &p.provided, new);
    let (predicted, release) = match prediction {
        Prediction::Requirements {
            version,
            requirements,
        } => (
            depreach::predicted_need(requirements, &p.package, &p.names, old, new),
            version.as_str(),
        ),
        Prediction::Unknown | Prediction::Failed(_) => (None, ""),
    };
    let (need, from_prediction) = depreach::combined_need(spec, predicted);
    let named = |text: String| {
        if from_prediction {
            format!("{text} (PyPI's Requires-Dist of {parent} {release})")
        } else {
            text
        }
    };
    match need {
        DepNeed::Update(t) => DepNeed::Update(named(t)),
        DepNeed::Unmet(t) => DepNeed::Unmet(named(t)),
        DepNeed::Unknown(t) => DepNeed::Unknown(named(t)),
        DepNeed::Origin => DepNeed::Origin,
    }
}

/// A queued package of the source project the package waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WaitFor {
    pub(super) dep: String,
    /// Its job's key, for the package's `after`.
    pub(super) key: String,
    /// The requirement its update meets, for the package's request: only
    /// for [`SameNeed::Needs`].
    pub(super) required: Option<String>,
    /// The version the source has, for the request's `origin_version`.
    pub(super) origin_version: String,
}

/// What [`same_project_after`] orders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SameOrder {
    pub(super) waits: Vec<WaitFor>,
    /// What the package does without a dependency it needs, one line each.
    pub(super) notes: Vec<String>,
}

/// The queued packages of the source `parent` waits for, given its `needs`;
/// `queued` holds the keys of every job pushed to `ready` so far.
///
/// It waits for every found dependency that is queued, needed or not: its
/// branch hides the source's binaries from the branch project until it has
/// built (as `DepQueue::add` waits for a dependency used from its origin).
/// No cycle can come of it: the scheduler only waits for jobs handed over
/// before, and `parent` is not queued yet.
///
/// A needed update that is not queued (it was skipped, or failed to branch)
/// is not waited for: `parent` builds against the source's version, which a
/// note says, as it does for each need that is left unmet.
pub(super) fn same_project_after(
    parent: &str,
    needs: &[NeedSameProject],
    index: &FoundIndex,
    queued: &HashSet<String>,
) -> SameOrder {
    let mut order = SameOrder::default();
    for n in needs {
        let found = index.get(&n.dep).filter(|f| queued.contains(&f.key));
        let dep = &n.dep;
        match (&n.need, found) {
            (SameNeed::Needs { bound, .. } | SameNeed::Unknown { bound, .. }, None) => {
                order.notes.push(format!(
                    "{parent} needs {bound}, but this run does not build the update of {dep}: \
                     {parent} builds against {dep} as the source has it"
                ));
            }
            (SameNeed::Unmet { new_version, bound }, _) => order.notes.push(format!(
                "{parent} needs {bound}, which neither {dep} as the source has it nor \
                 {new_version} meets; the build will say what is missing"
            )),
            (SameNeed::NoUpdate { bound }, _) => order.notes.push(format!(
                "{parent} needs {bound}, and this run found no update of {dep}: {parent} builds \
                 against {dep} as the source has it"
            )),
            _ => {}
        }
        let Some(f) = found else { continue };
        if order.waits.iter().any(|w| w.key == f.key) {
            continue;
        }
        let required = match &n.need {
            SameNeed::Needs { bound, .. } => Some(bound.clone()),
            _ => None,
        };
        order.waits.push(WaitFor {
            dep: f.pkg.clone(),
            key: f.key.clone(),
            required,
            origin_version: f.old_version.clone(),
        });
    }
    order
}
