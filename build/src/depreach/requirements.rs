//! Capability BuildRequires (`pkgconfig(x) >= V`) checked against the RPMs
//! the branch's build paths reach, per repository it builds in. The RPMs are
//! found by a binary search for the capability's word, and, for a project the
//! search does not index, by its repository's file listing; each one read for
//! its `Provides:`. The decision is [`judge_capability`]. The branch's build
//! flags decide which repositories count (see [`build_disabled_in`]).
//!
//! The lookups are bounded: at most [`CAPABILITY_READS`] RPMs are read per
//! capability and repository, and the time of the fetches is counted against
//! [`CAPABILITY_BUDGET`] for the run (see [`DepResolver::charged`]). Once it is
//! spent, the capability being fetched is unchecked, and so is every later one.
//! Nothing here adds a build path.

use std::thread;

use super::*;
use crate::spec::Requirement;

pub(super) fn search_key(needle: &str, projects: &[String], arch: &str) -> String {
    format!("{needle}|{}|{arch}", projects.join(","))
}

pub(super) fn provides_key(project: &str, repository: &str, arch: &str, name: &str) -> String {
    format!("{project}|{repository}|{arch}|{name}")
}

pub(super) fn listing_key(project: &str, repository: &str, arch: &str) -> String {
    format!("{project}|{repository}|{arch}")
}

/// The requirement as the spec writes it, with its bound (`pkgconfig(x) >= 1`).
pub(super) fn requirement_text(req: &Requirement, capability: &str) -> String {
    match &req.bound {
        Some(bound) => format!("{capability} {} {}", bound.op, bound.version),
        None => capability.to_string(),
    }
}

/// Why a capability is unchecked once the run's capability budget is spent.
const BUDGET_SPENT: &str = "capability check budget spent";

/// The name inside a capability: `wayland-client` for `pkgconfig(wayland-client)`.
fn capability_name(capability: &str) -> &str {
    capability
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or(capability)
}

/// Whether a package called `pkg` may provide `capability`, whose word is
/// `needle`: the package is named for the word (`wayland`, `libwayland`, each
/// with or without `-devel`) or for the capability's own name. Erring this way
/// only leaves a capability unchecked.
pub(super) fn may_be(pkg: &str, needle: &str, capability: &str) -> bool {
    let base = pkg.strip_suffix("-devel").unwrap_or(pkg);
    pkg == capability_name(capability) || base == needle || base.strip_prefix("lib") == Some(needle)
}

/// The order candidates are read in: names that start with the word first,
/// then `-devel` packages, then by project and name.
fn read_order(a: &osc::CandidateRpm, b: &osc::CandidateRpm, needle: &str) -> std::cmp::Ordering {
    let rank = |r: &osc::CandidateRpm| (!r.name.starts_with(needle), !r.name.contains("devel"));
    rank(a)
        .cmp(&rank(b))
        .then_with(|| (&a.project, &a.name).cmp(&(&b.project, &b.name)))
}

impl DepResolver {
    /// Judge each capability requirement in `reqs` for each repository the
    /// branch builds in that its build flags do not leave out. The flags are
    /// those of the branch package (or of `origin` while the branch does not
    /// have it), then the branch project's. `run_packages` are the packages
    /// this update has handled so far: a capability one of them may provide
    /// is unchecked. Packages are not judged here: see [`DepResolver::check`].
    pub fn check_requirements(
        &self,
        reqs: &[Requirement],
        package: &str,
        origin: &str,
        run_packages: &[String],
    ) -> Result<Vec<DepVerdict>, String> {
        let caps: Vec<(&Requirement, &str)> = reqs
            .iter()
            .filter_map(|r| match &r.dep {
                BuildRequire::Capability(c) => Some((r, c.as_str())),
                BuildRequire::Package(_) => None,
            })
            .collect();
        if caps.is_empty() {
            return Ok(Vec::new());
        }
        let (branch_meta, info) = self.branch_info()?;
        let building: Vec<String> = info
            .repositories
            .iter()
            .filter(|r| r.arches.contains(&self.arch))
            .map(|r| r.name.clone())
            .collect();
        let names: Vec<&str> = building.iter().map(String::as_str).collect();
        self.warm_path_metas(&info, &names);
        let flags = self.flags_meta(package, origin);
        let levels: Vec<&str> = flags
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(branch_meta.as_str()))
            .collect();
        let mut repos: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for repo in &building {
            if build_disabled_in(&levels, repo, &self.arch) {
                continue;
            }
            let mut pairs = vec![(self.branch_project.clone(), repo.clone())];
            pairs.extend(reachable_pairs(
                &self.branch_project,
                &info,
                repo,
                &mut |p| self.path_project_info(p),
            ));
            repos.push((repo.clone(), pairs));
        }
        Ok(caps
            .into_iter()
            .map(|(req, cap)| DepVerdict {
                name: requirement_text(req, cap),
                state: DepState::Capability(self.judge_one(req, cap, &repos, run_packages)),
            })
            .collect())
    }

    /// The build flags of the branch package, or of `origin` when the branch
    /// does not have it. `None` (every repository judged) when neither reads.
    fn flags_meta(&self, package: &str, origin: &str) -> Option<String> {
        let read = |project: &str| self.obs.package_meta(project, package);
        let meta = read(&self.branch_project).and_then(|meta| match meta {
            Some(meta) => Ok(Some(meta)),
            None => read(origin),
        });
        match meta {
            Ok(meta) => meta,
            Err(e) => {
                self.warn(format!(
                    "cannot read the build flags of {package}, so every repository is judged: {e}"
                ));
                None
            }
        }
    }

    /// The packages of the branch project, read once per run.
    fn branch_package_names(&self) -> Result<HashSet<String>, String> {
        let mut cache = self
            .branch_packages
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cache.is_none() {
            *cache = Some(self.obs.project_packages(&self.branch_project));
        }
        cache
            .clone()
            .unwrap_or_else(|| Err("no listing".to_string()))
    }

    fn judge_one(
        &self,
        req: &Requirement,
        cap: &str,
        repos: &[(String, Vec<(String, String)>)],
        run_packages: &[String],
    ) -> CapState {
        let Some(needle) = name_needle(cap) else {
            return CapState::Unchecked(format!(
                "{cap} is not looked up by name (a macro, a file, or capitals)"
            ));
        };
        if self.budget_spent() {
            return CapState::Unchecked(BUDGET_SPENT.to_string());
        }
        if let Some(pkg) = run_packages
            .iter()
            .filter(|p| may_be(p, &needle, cap))
            .min()
        {
            return CapState::Unchecked(format!("may be provided by {pkg} (updated in this run)"));
        }
        match self.branch_package_names() {
            Ok(names) => {
                if let Some(pkg) = names.iter().filter(|p| may_be(p, &needle, cap)).min() {
                    return CapState::Unchecked(format!(
                        "may be provided by {pkg} (in {})",
                        self.branch_project
                    ));
                }
            }
            Err(why) => return CapState::Unchecked(why),
        }
        let _fetching = self
            .capability_fetch
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let projects: Vec<String> = {
            let mut all: Vec<String> = Vec::new();
            for (_, pairs) in repos {
                for (project, _) in pairs {
                    if !all.contains(project) {
                        all.push(project.clone());
                    }
                }
            }
            all
        };
        let rpms = match self.reached_rpms(cap, &needle, &projects, repos) {
            Ok(rpms) => rpms,
            Err(why) => return CapState::Unchecked(why),
        };
        // Per repository, the first RPMs its paths reach, in read order.
        let mut reaches: Vec<Reach> = Vec::new();
        let mut selected: Vec<osc::CandidateRpm> = Vec::new();
        for (repo, pairs) in repos {
            let reach_projects: Vec<String> = pairs.iter().map(|(p, _)| p.clone()).collect();
            let mut reached: Vec<osc::CandidateRpm> = rpms
                .iter()
                .filter(|r| reach_projects.contains(&r.project))
                .cloned()
                .collect();
            reached.sort_by(|a, b| read_order(a, b, &needle));
            let unread = reached.len().saturating_sub(CAPABILITY_READS);
            reached.truncate(CAPABILITY_READS);
            for r in reached {
                let known = selected.iter().any(|s| {
                    (&s.project, &s.repository, &s.name) == (&r.project, &r.repository, &r.name)
                });
                if !known {
                    selected.push(r);
                }
            }
            reaches.push(Reach {
                repo: repo.clone(),
                projects: reach_projects,
                unread,
            });
        }
        let candidates = self.read_provides(&selected);
        // A read skipped for the budget leaves its candidate out, so the
        // verdict would rest on too little: the capability is unchecked.
        if self.budget_spent() {
            return CapState::Unchecked(BUDGET_SPENT.to_string());
        }
        judge_capability(req, cap, &reaches, &candidates)
    }

    /// The RPMs that may provide `cap` in the projects `repos` reach: the
    /// search hits, and for each project the search does not index (none of
    /// its RPMs were found), the names its repository lists. The caller holds
    /// `capability_fetch`.
    fn reached_rpms(
        &self,
        cap: &str,
        needle: &str,
        projects: &[String],
        repos: &[(String, Vec<(String, String)>)],
    ) -> Result<Vec<osc::CandidateRpm>, String> {
        let found = self.charged(|| self.rpm_search(needle, projects))?;
        if self.budget_spent() {
            return Err(BUDGET_SPENT.to_string());
        }
        if found.truncated {
            return Err(format!(
                "OBS limited the search for {needle}, so {cap} is not judged"
            ));
        }
        let mut rpms: Vec<osc::CandidateRpm> = found
            .rpms
            .into_iter()
            .filter(|r| may_provide(cap, &r.name))
            .collect();
        let indexed: Vec<String> = rpms.iter().map(|r| r.project.clone()).collect();
        let mut listed: Vec<(String, String)> = Vec::new();
        for (_, pairs) in repos {
            for (project, repo) in pairs {
                if !indexed.contains(project) && !listed.contains(&(project.clone(), repo.clone()))
                {
                    listed.push((project.clone(), repo.clone()));
                }
            }
        }
        // The branch's own RPMs are covered by its package names, which are
        // checked first, and the branch may not exist yet.
        listed.retain(|(project, _)| *project != self.branch_project);
        self.prefetch_listings(&listed);
        for (project, repo) in listed {
            if self.budget_spent() {
                return Err(BUDGET_SPENT.to_string());
            }
            let names = self.charged(|| self.repository_names(&project, &repo))?;
            // Case-insensitive: RPM names keep the capitals of their upstream
            // name (`libX11-devel` for the word `x11`).
            rpms.extend(
                names
                    .into_iter()
                    .filter(|n| n.to_ascii_lowercase().contains(needle) && may_provide(cap, n))
                    .map(|name| osc::CandidateRpm {
                        name,
                        project: project.clone(),
                        repository: repo.clone(),
                        arch: self.arch.clone(),
                        package: None,
                    }),
            );
        }
        let mut seen = HashSet::new();
        rpms.retain(|r| seen.insert(provides_key(&r.project, &r.repository, &r.arch, &r.name)));
        Ok(rpms)
    }

    /// A capability search, made once per run.
    fn rpm_search(&self, needle: &str, projects: &[String]) -> Result<osc::RpmSearch, String> {
        let key = search_key(needle, projects, &self.arch);
        let cached = self
            .rpm_searches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        if let Some(answer) = cached {
            return answer;
        }
        let answer = self.obs.rpms_containing(needle, projects, &self.arch);
        self.rpm_searches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, answer.clone());
        answer
    }

    /// List the repositories not listed yet, in batches of
    /// [`FETCH_PARALLELISM`] requests. The caller holds `capability_fetch`.
    fn prefetch_listings(&self, pairs: &[(String, String)]) {
        let todo: Vec<&(String, String)> = {
            let cache = self.listings.lock().unwrap_or_else(|e| e.into_inner());
            pairs
                .iter()
                .filter(|(p, r)| !cache.contains_key(&listing_key(p, r, &self.arch)))
                .collect()
        };
        for batch in todo.chunks(FETCH_PARALLELISM) {
            if self.budget_spent() {
                break;
            }
            let results: Vec<Result<Vec<String>, String>> = self.charged(|| {
                thread::scope(|scope| {
                    let handles: Vec<_> = batch
                        .iter()
                        .map(|(p, r)| scope.spawn(|| self.obs.repository_names(p, r, &self.arch)))
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join()
                                .unwrap_or_else(|_| Err("listing panicked".to_string()))
                        })
                        .collect()
                })
            });
            let mut cache = self.listings.lock().unwrap_or_else(|e| e.into_inner());
            for ((p, r), names) in batch.iter().zip(results) {
                cache.insert(listing_key(p, r, &self.arch), names);
            }
        }
    }

    /// The binary names of a repository, listed once per run.
    fn repository_names(&self, project: &str, repository: &str) -> Result<Vec<String>, String> {
        let key = listing_key(project, repository, &self.arch);
        let cached = self
            .listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        if let Some(names) = cached {
            return names;
        }
        let names = self.obs.repository_names(project, repository, &self.arch);
        self.listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, names.clone());
        names
    }

    /// The RPMs as candidates, their `Provides:` read once per run, in batches
    /// of [`FETCH_PARALLELISM`] requests. The caller holds `capability_fetch`.
    fn read_provides(&self, rpms: &[osc::CandidateRpm]) -> Vec<Candidate> {
        let todo: Vec<&osc::CandidateRpm> = {
            let cache = self.provides.lock().unwrap_or_else(|e| e.into_inner());
            let mut seen = HashSet::new();
            rpms.iter()
                .filter(|r| {
                    let key = provides_key(&r.project, &r.repository, &r.arch, &r.name);
                    !cache.contains_key(&key) && seen.insert(key)
                })
                .collect()
        };
        for batch in todo.chunks(FETCH_PARALLELISM) {
            // Past the budget no further batch is started; the caller then
            // leaves the capability unchecked.
            if self.budget_spent() {
                break;
            }
            let results: Vec<Result<Vec<String>, String>> = self.charged(|| {
                thread::scope(|scope| {
                    let handles: Vec<_> = batch
                        .iter()
                        .map(|r| {
                            scope.spawn(|| {
                                self.obs.rpm_provides(
                                    &r.project,
                                    &r.repository,
                                    &self.arch,
                                    &r.name,
                                )
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join()
                                .unwrap_or_else(|_| Err("provides read panicked".to_string()))
                        })
                        .collect()
                })
            });
            let mut cache = self.provides.lock().unwrap_or_else(|e| e.into_inner());
            for (r, provides) in batch.iter().zip(results) {
                let key = provides_key(&r.project, &r.repository, &r.arch, &r.name);
                cache.insert(key, provides);
            }
        }
        let cache = self.provides.lock().unwrap_or_else(|e| e.into_inner());
        rpms.iter()
            .filter_map(|r| {
                let key = provides_key(&r.project, &r.repository, &r.arch, &r.name);
                let provides = cache.get(&key)?.clone();
                Some(Candidate {
                    name: r.name.clone(),
                    hit: Hit {
                        project: r.project.clone(),
                        repository: r.repository.clone(),
                        package: r.package.clone(),
                        version: None,
                    },
                    provides,
                })
            })
            .collect()
    }
}
