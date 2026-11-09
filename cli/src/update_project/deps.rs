//! Which dependencies `--update-deps` branches and builds ahead of the
//! package that needs them.
//!
//! A dependency that is not branched resolves from its origin through the
//! branch project's repository paths, so by default the package is tested
//! against the origin's binaries, and a dependency is updated first only when
//! the package's BuildRequires need a version its origin does not have. A
//! bound that cannot be compared with the versions also updates it, since the
//! bound may need the update. `--test-against-updates` updates every dependency that has a newer release.
//!
//! A branched dependency is the branch project's own: until it has built,
//! it hides the origin's binaries from every package of the branch, so a
//! package that uses it waits for its build.

use super::*;
use ap_build::depreach::DepNeed;
use ap_build::spec::Requirement;
use ap_build::update_project::RequiredUpdate;

/// What becomes of a dependency with a newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DepChoice {
    /// It is branched and built first. The update the package's requirements
    /// need, when they need one (`--test-against-updates` also updates a
    /// dependency they do not need newer).
    Update(Option<RequiredUpdate>),
    /// It is branched and built first, though no bound on it could be
    /// compared with its versions, so the update may be needed. The reason
    /// given says which bound.
    UpdateUnchecked(String),
    /// It is used as its origin publishes it, for the reason given.
    Origin(String),
}

/// What becomes of `dep`, a dependency of `parent` that `origin` publishes
/// (`origin.names` are what `parent` requires it by), given `parent`'s
/// requirements `reqs`. `branch` is the branch project.
pub(super) fn choose_dependency(
    test_against_updates: bool,
    reqs: &[Requirement],
    origin: &depreach::DependencyOrigin,
    dep: &Item,
    parent: &str,
    branch: &str,
) -> DepChoice {
    let need = depreach::dependency_need(
        reqs,
        &origin.names,
        &dep.old_version,
        &origin.provided,
        &dep.new_version,
    );
    let required = |requirement: String| RequiredUpdate {
        pkg: dep.pkg.clone(),
        branch_package: format!("{branch}/{}", dep.pkg),
        requirement,
        origin: dep.origin.clone(),
        origin_version: dep.old_version.clone(),
        request: None,
    };
    let (pkg, origin, old, new) = (&dep.pkg, &dep.origin, &dep.old_version, &dep.new_version);
    match need {
        DepNeed::Update(requirement) => DepChoice::Update(Some(required(requirement))),
        _ if test_against_updates => DepChoice::Update(None),
        DepNeed::Origin => DepChoice::Origin(format!(
            "{parent} builds against {pkg} {old} from {origin}: its BuildRequires do not need \
             {new} (--test-against-updates updates it first)"
        )),
        DepNeed::Unmet(requirement) => DepChoice::Origin(format!(
            "{parent} needs {requirement}, which neither {pkg} {old} in {origin} nor {new} \
             meets; not updating {pkg}, the build will say what is missing"
        )),
        DepNeed::Unknown(requirement) => DepChoice::UpdateUnchecked(format!(
            "{parent} needs {requirement}, which cannot be compared with {pkg} {old} or {new}; \
             updating {pkg} from {origin} first, since the bound may need it"
        )),
    }
}

/// The run's queue of dependencies, as `--update-deps` grows it.
pub(super) struct DepQueue<'q> {
    pub(super) queue: &'q mut VecDeque<Item>,
    pub(super) visited: &'q mut HashSet<String>,
    pub(super) count: &'q mut usize,
}

impl DepQueue<'_> {
    /// Queue `dep`, chosen for `item`, ahead of it; `after` gets each
    /// dependency `item` waits for. A dependency used from its origin is not
    /// queued, but `item` still waits for it when this run already branches
    /// it for another package, since that branch hides the origin's binaries.
    pub(super) fn add(
        &mut self,
        item: &mut Item,
        dep: Item,
        choice: DepChoice,
        after: &mut Vec<String>,
        reporter: &dyn Reporter,
    ) {
        let say = |message: String| {
            reporter.report(&Event::Progress {
                step: None,
                message,
            })
        };
        let warn = |message: String| {
            reporter.report(&Event::Warning {
                step: None,
                message,
            })
        };
        let key = visit_key(&dep);
        let (required, unchecked) = match choice {
            DepChoice::Origin(why) => {
                if self.visited.contains(&key) {
                    say(format!(
                        "{}: waits for {}, which this run branches for another package",
                        item.pkg, dep.pkg
                    ));
                    after.push(key);
                } else {
                    say(why);
                }
                return;
            }
            DepChoice::Update(required) => (required, None),
            DepChoice::UpdateUnchecked(why) => (None, Some(why)),
        };
        // Built before this item, also when an earlier item queued it. Only a
        // dependency that is queued (or was, already) is waited for: a key
        // that is never queued would never be handed over.
        if self.visited.contains(&key) {
            after.push(key);
            item.requires.extend(required);
            return;
        }
        if *self.count >= MAX_DEPENDENCY_ITEMS {
            warn(format!(
                "--update-deps: not updating {} too, the limit is {MAX_DEPENDENCY_ITEMS}",
                dep.pkg
            ));
            return;
        }
        after.push(key.clone());
        self.visited.insert(key);
        *self.count += 1;
        match (&required, unchecked) {
            (Some(r), _) => say(format!(
                "{} needs {}, and {} has {} {}: updating {} to {} first",
                item.pkg,
                r.requirement,
                dep.origin,
                dep.pkg,
                dep.old_version,
                dep.pkg,
                dep.new_version
            )),
            (None, Some(why)) => say(why),
            (None, None) => say(format!(
                "{} needs {} from {}, which has a newer release ({} -> {})",
                item.pkg, dep.pkg, dep.origin, dep.old_version, dep.new_version
            )),
        }
        item.requires.extend(required);
        self.queue.push_back(dep);
    }
}
