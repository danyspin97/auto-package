use std::time::{Duration, Instant};

use super::*;

/// Reachability checks and path edits for one branch project and
/// architecture, with the OBS answers cached for the run.
pub struct DepResolver {
    pub(super) obs: Box<dyn Obs>,
    reporter: Arc<dyn Reporter + Send + Sync>,
    pub(super) branch_project: String,
    pub(super) arch: String,
    metas: Mutex<HashMap<String, Option<ProjectInfo>>>,
    pub(super) binaries: Mutex<HashMap<String, Vec<Hit>>>,
    /// Held across a meta read-modify-write, so two edits cannot interleave.
    edit: Mutex<()>,
    /// Held while fetching binaries, so callers with overlapping names fetch
    /// each once: the later one finds them cached when its turn comes.
    binaries_fetch: Mutex<()>,
    /// The same for path-project metas.
    metas_fetch: Mutex<()>,
    /// Set for a maintenance run: no build path is added, and this says why.
    frozen_paths: Option<String>,
    /// Capability searches made, by [`requirements::search_key`].
    pub(super) rpm_searches: Mutex<HashMap<String, Result<osc::RpmSearch, String>>>,
    /// `Provides:` of each RPM read, by [`requirements::provides_key`].
    pub(super) provides: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    /// Binary names each repository lists, by [`requirements::listing_key`].
    pub(super) listings: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    /// Held across the capability fetches, so callers with overlapping
    /// lookups fetch each RPM once: the later one finds it cached.
    pub(super) capability_fetch: Mutex<()>,
    /// The packages of the branch project, read once per run.
    pub(super) branch_packages: Mutex<Option<Result<HashSet<String>, String>>>,
    /// Time the capability fetches of the run have taken so far, see
    /// [`DepResolver::charged`].
    capability_spent: Mutex<Duration>,
    /// What the fetch time is measured by: the run's clock, unless a test
    /// sets its own.
    clock: Box<dyn Fn() -> Duration + Send + Sync>,
    pub(super) budget: Duration,
}

/// Requests in flight at once when fetching several chunks or metas.
pub(super) const FETCH_PARALLELISM: usize = 8;

/// How long the capability fetches of one run may take in all. Only the time
/// the fetches take is counted, not the OBS calls around them. Once spent,
/// every capability still to be checked is unchecked.
pub(super) const CAPABILITY_BUDGET: Duration = Duration::from_secs(120);

/// How many RPMs are read per capability and repository. Past it, a verdict
/// that rests on the RPMs read alone is unchecked, not a warning.
pub(super) const CAPABILITY_READS: usize = 8;

impl DepResolver {
    pub fn new(
        obs: Box<dyn Obs>,
        reporter: Arc<dyn Reporter + Send + Sync>,
        branch_project: &str,
        arch: &str,
    ) -> Self {
        let origin = Instant::now();
        DepResolver {
            obs,
            reporter,
            branch_project: branch_project.to_string(),
            arch: arch.to_string(),
            metas: Mutex::default(),
            binaries: Mutex::default(),
            edit: Mutex::new(()),
            binaries_fetch: Mutex::new(()),
            metas_fetch: Mutex::new(()),
            frozen_paths: None,
            rpm_searches: Mutex::default(),
            provides: Mutex::default(),
            listings: Mutex::default(),
            capability_fetch: Mutex::new(()),
            branch_packages: Mutex::new(None),
            capability_spent: Mutex::new(Duration::ZERO),
            clock: Box::new(move || origin.elapsed()),
            budget: CAPABILITY_BUDGET,
        }
    }

    /// The same resolver with a different capability budget.
    pub fn with_capability_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// The same resolver with `clock` measuring the capability fetches.
    #[cfg(test)]
    pub(super) fn with_capability_clock(
        mut self,
        clock: Box<dyn Fn() -> Duration + Send + Sync>,
    ) -> Self {
        self.clock = clock;
        self
    }

    /// Run `f` and count the time it takes against the capability budget. Only
    /// the capability fetches are run this way, so the OBS calls around them
    /// (metas, the branch's package list) are not counted.
    pub(super) fn charged<T>(&self, f: impl FnOnce() -> T) -> T {
        let before = (self.clock)();
        let out = f();
        let took = (self.clock)().saturating_sub(before);
        *self
            .capability_spent
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += took;
        out
    }

    /// Whether the capability fetches of the run have used up the budget.
    pub(super) fn budget_spent(&self) -> bool {
        *self
            .capability_spent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            >= self.budget
    }

    /// A resolver that checks reachability but adds no build path: a path
    /// [`DepResolver::add_paths`] would add is refused with `reason` instead.
    /// Used by maintenance runs, whose incident builds against the code
    /// stream only.
    pub fn with_frozen_paths(mut self, reason: &str) -> Self {
        self.frozen_paths = Some(reason.to_string());
        self
    }

    /// The meta of a project other than the branch project, from the cache
    /// when it was read before. Unreadable counts as absent.
    pub(super) fn path_project_info(&self, project: &str) -> Option<ProjectInfo> {
        if let Some(cached) = self
            .metas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(project)
        {
            return cached.clone();
        }
        let info = match self.obs.project_meta(project) {
            Ok(meta) => meta.map(|m| osc::parse_project_info(&m)),
            Err(e) => {
                self.warn(format!(
                    "cannot read the meta of path project {project}: {e}"
                ));
                None
            }
        };
        self.metas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(project.to_string(), info.clone());
        info
    }

    pub(super) fn warn(&self, message: String) {
        self.reporter.report(&Event::Warning {
            step: None,
            message,
        });
    }

    fn say(&self, message: String) {
        self.reporter.report(&Event::Progress {
            step: None,
            message,
        });
    }

    /// The branch project's meta, always fresh: it is what gets edited.
    pub(super) fn branch_info(&self) -> Result<(String, ProjectInfo), String> {
        let meta = self
            .obs
            .project_meta(&self.branch_project)?
            .ok_or_else(|| format!("branch project {} does not exist", self.branch_project))?;
        let info = osc::parse_project_info(&meta);
        Ok((meta, info))
    }

    /// Fetch and cache the binaries of every name not asked about yet. A name
    /// that cannot be searched is not fetched and not cached: it stays unknown.
    fn prefetch(&self, names: &[&str]) -> Result<(), String> {
        let _turn = self
            .binaries_fetch
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let todo: Vec<&str> = {
            let cache = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
            let mut seen = HashSet::new();
            names
                .iter()
                .copied()
                .filter(|n| {
                    osc::is_searchable_binary_name(n) && !cache.contains_key(*n) && seen.insert(*n)
                })
                .collect()
        };
        let chunks: Vec<&[&str]> = todo.chunks(SEARCH_CHUNK).collect();
        for batch in chunks.chunks(FETCH_PARALLELISM) {
            let results: Vec<Result<Vec<PublishedBinary>, String>> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|chunk| scope.spawn(|| self.obs.published_binaries(chunk)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err("binary search panicked".to_string()))
                    })
                    .collect()
            });
            for (chunk, found) in batch.iter().zip(results) {
                let found = found?;
                let mut cache = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
                for name in *chunk {
                    let hits: Vec<Hit> = found
                        .iter()
                        .filter(|b| b.name == *name && b.arch == self.arch)
                        .map(Hit::of)
                        .collect();
                    cache.insert(name.to_string(), hits);
                }
            }
        }
        Ok(())
    }

    /// Read the metas of the projects `repositories` of `info` reach through
    /// their paths, a level at a time with each level fetched in parallel, so
    /// the walk in [`reachable_projects`] finds them all cached.
    pub(super) fn warm_path_metas(&self, info: &ProjectInfo, repositories: &[&str]) {
        let _turn = self.metas_fetch.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut level: Vec<(String, String)> = info
            .repositories
            .iter()
            .filter(|r| repositories.contains(&r.name.as_str()))
            .flat_map(|r| r.paths.clone())
            .collect();
        while !level.is_empty() {
            level
                .retain(|node| seen.len() < osc::MAX_BUILD_PATH_NODES && seen.insert(node.clone()));
            let missing: Vec<&str> = {
                let metas = self.metas.lock().unwrap_or_else(|e| e.into_inner());
                let mut unique = HashSet::new();
                level
                    .iter()
                    .map(|(p, _)| p.as_str())
                    .filter(|p| !metas.contains_key(*p) && unique.insert(*p))
                    .collect()
            };
            for batch in missing.chunks(FETCH_PARALLELISM) {
                std::thread::scope(|scope| {
                    for project in batch {
                        scope.spawn(|| self.path_project_info(project));
                    }
                });
            }
            let metas = self.metas.lock().unwrap_or_else(|e| e.into_inner());
            level = level
                .iter()
                .filter_map(|(project, repo)| Some((metas.get(project)?.as_ref()?, repo)))
                .flat_map(|(info, repo)| {
                    info.repositories
                        .iter()
                        .filter(move |r| &r.name == repo)
                        .flat_map(|r| r.paths.clone())
                })
                .collect();
        }
    }

    /// Check each dependency against what the branch project's paths reach.
    pub fn check(&self, deps: &[BuildRequire]) -> Result<Vec<DepVerdict>, String> {
        let names: Vec<&str> = deps
            .iter()
            .filter_map(|d| match d {
                BuildRequire::Package(n) => Some(n.as_str()),
                BuildRequire::Capability(_) => None,
            })
            .collect();
        self.prefetch(&names)?;

        let (_, info) = self.branch_info()?;
        let mut repos: Vec<(String, Vec<String>)> = Vec::new();
        let building: Vec<&str> = info
            .repositories
            .iter()
            .filter(|r| r.arches.contains(&self.arch))
            .map(|r| r.name.as_str())
            .collect();
        self.warm_path_metas(&info, &building);
        for repo in info
            .repositories
            .iter()
            .filter(|r| r.arches.contains(&self.arch))
        {
            let reach = reachable_projects(&self.branch_project, &info, &repo.name, &mut |p| {
                self.path_project_info(p)
            });
            repos.push((repo.name.clone(), reach));
        }
        if repos.is_empty() {
            return Err(format!(
                "{} has no repository building for {}",
                self.branch_project, self.arch
            ));
        }
        // Judged under the lock rather than on a copy of the whole cache.
        let hits = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
        // Capabilities are judged by `check_requirements`.
        let packages: Vec<BuildRequire> = deps
            .iter()
            .filter(|d| matches!(d, BuildRequire::Package(_)))
            .cloned()
            .collect();
        Ok(judge(&packages, &repos, &hits, &self.branch_project))
    }

    /// Add `additions` to the branch project's meta, skipping paths it already
    /// has, and report each one added. Returns what was added.
    pub fn add_paths(&self, additions: &[PathAddition]) -> Result<Vec<PathAddition>, String> {
        if additions.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.edit.lock().unwrap_or_else(|e| e.into_inner());
        let (meta, _) = self.branch_info()?;
        let (edited, made) = osc::add_paths_to_meta(&meta, additions)?;
        if made.is_empty() {
            return Ok(made);
        }
        if let Some(reason) = &self.frozen_paths {
            let wanted: Vec<String> = made
                .iter()
                .map(|a| format!("{}/{}", a.project, a.path_repository))
                .collect();
            return Err(format!("{reason}; it would need {}", wanted.join(", ")));
        }
        self.obs.set_project_meta(&self.branch_project, &edited)?;
        for a in &made {
            self.say(format!(
                "added build path {}/{} to {} (repository {})",
                a.project, a.path_repository, self.branch_project, a.repository
            ));
        }
        Ok(made)
    }

    /// For a dependency the pipeline found missing: make it reachable, or say
    /// that it already is, instead of building a second copy of it. `candidates`
    /// are the binary names it might be published under. `None` when nothing
    /// publishes any of them or a path cannot be added.
    pub fn cover_dependency(&self, candidates: &[String]) -> Result<Option<String>, String> {
        let deps: Vec<BuildRequire> = candidates
            .iter()
            .map(|c| BuildRequire::Package(c.clone()))
            .collect();
        let verdicts = self.check(&deps)?;
        if let Some(v) = verdicts.iter().find_map(|v| match &v.state {
            DepState::Reachable { provider } => Some((v, provider)),
            _ => None,
        }) {
            return Ok(Some(format!(
                "{} is published by {}, which {} already builds against",
                v.0.name, v.1.project, self.branch_project
            )));
        }
        let missing: Vec<DepVerdict> = verdicts
            .into_iter()
            .filter(|v| matches!(v.state, DepState::Missing { .. }))
            .collect();
        // One candidate name is enough: they are alternative spellings of the
        // same dependency.
        let Some(first) = missing.first() else {
            return Ok(None);
        };
        let additions = choose_paths(std::slice::from_ref(first));
        if additions.is_empty() {
            return Ok(None);
        }
        self.add_paths(&additions)?;
        let projects: Vec<&str> = additions.iter().map(|a| a.project.as_str()).collect();
        Ok(Some(format!(
            "{} is published by {}; added it as a build path of {}",
            first.name,
            projects.join(", "),
            self.branch_project
        )))
    }
}
