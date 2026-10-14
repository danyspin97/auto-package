//! Whether a branch project's build paths can supply a package's
//! `BuildRequires`, and which extra path to add when they cannot.
//!
//! A package branched out of, say, `Base:System` may need packages that live
//! in another project (`devel:languages:c_c++`). The branch project is built
//! against its repository paths, so such a package is only found if one of the
//! paths — directly or through the paths of the paths — publishes it.
//!
//! The check asks OBS for the published binaries of each name once (a search
//! across every project, cached for the run) and compares the projects that
//! publish it with the projects the branch project's paths reach. Reachability
//! is decided per project, not per repository: a distro project publishes
//! under repository names (`standard`, `snapshot`) that its dependents name
//! differently, and what matters is that the project is on the path chain.
//!
//! Only a name that is a binary package name can be looked up this way. A
//! capability (`pkgconfig(glib-2.0)`) is reported as unchecked; the build
//! catches those, and `cover_dependency` handles them afterwards.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ap_core::event::Event;
use ap_core::sink::Reporter;

use crate::osc::{self, PathAddition, ProjectInfo, PublishedBinary};
use crate::proc::Emit;
use crate::spec::BuildRequire;

/// How many names one search request carries.
const SEARCH_CHUNK: usize = 20;
/// Bound on the path graph walked from a repository, against a cyclic or
/// absurdly deep chain.
const MAX_PATH_NODES: usize = 64;

/// What the resolver asks of OBS. A trait so the logic runs against fixtures.
pub trait Obs: Send + Sync {
    /// The meta XML of a project, `None` when it does not exist.
    fn project_meta(&self, project: &str) -> Result<Option<String>, String>;
    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String>;
    /// The published binaries named any of `names`, in any project.
    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String>;
}

/// [`Obs`] over `osc`.
pub struct LiveObs {
    pub reporter: Arc<dyn Reporter + Send + Sync>,
}

impl Obs for LiveObs {
    fn project_meta(&self, project: &str) -> Result<Option<String>, String> {
        osc::get_project_meta(project, Emit::run(self.reporter.as_ref())).map_err(|e| e.to_string())
    }

    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String> {
        osc::set_project_meta(project, xml, Emit::run(self.reporter.as_ref()))
            .map_err(|e| e.to_string())
    }

    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        osc::search_published_binaries(names, Emit::quiet()).map_err(|e| e.to_string())
    }
}

/// A project that publishes a binary, for one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub project: String,
    pub repository: String,
    /// The source package it comes from.
    pub package: Option<String>,
    pub version: Option<String>,
}

impl Hit {
    fn of(b: &PublishedBinary) -> Self {
        Hit {
            project: b.project.clone(),
            repository: b.repository.clone(),
            package: b.package.clone(),
            version: b.version.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepState {
    /// Every target repository of the branch project reaches a project that
    /// publishes it.
    Reachable { provider: Hit },
    /// Some repositories reach nothing that publishes it, though other
    /// projects do. For each such repository, the candidates that could be
    /// added as a path.
    Missing { options: Vec<(String, Vec<Hit>)> },
    /// Published, but only where a path cannot sensibly point: in a personal
    /// project, or under repository names the branch project has no match for.
    OnlyUnusable { hits: Vec<Hit> },
    /// No project publishes a binary of that name for the architecture. It may
    /// still be a virtual provide the distro supplies.
    NotPublished,
    /// Not a package name; see the module docs.
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepVerdict {
    pub name: String,
    pub state: DepState,
}

/// Whether `project` is a distribution project (`openSUSE:Factory`,
/// `SUSE:SLE-15`), which is only ever referenced, never updated.
pub fn is_distro_project(project: &str) -> bool {
    ["openSUSE:", "openSUSE.org:", "SUSE:"]
        .iter()
        .any(|p| project.starts_with(p))
}

/// The projects reachable from `repository` of a project, in path order, the
/// project itself first. `info_of` yields the meta of a path project, or
/// `None` for one that cannot be read, which then ends that branch of the walk.
fn reachable_projects(
    own: &str,
    own_info: &ProjectInfo,
    repository: &str,
    info_of: &mut dyn FnMut(&str) -> Option<ProjectInfo>,
) -> Vec<String> {
    let mut order = vec![own.to_string()];
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut queue: std::collections::VecDeque<(String, String)> = own_info
        .repositories
        .iter()
        .filter(|r| r.name == repository)
        .flat_map(|r| r.paths.clone())
        .collect();
    while let Some((project, repo)) = queue.pop_front() {
        if seen.len() >= MAX_PATH_NODES || !seen.insert((project.clone(), repo.clone())) {
            continue;
        }
        if !order.contains(&project) {
            order.push(project.clone());
        }
        if let Some(info) = info_of(&project) {
            queue.extend(
                info.repositories
                    .iter()
                    .filter(|r| r.name == repo)
                    .flat_map(|r| r.paths.clone()),
            );
        }
    }
    order
}

/// Classify each dependency against the projects `reach` (per target
/// repository) and what OBS publishes. `hits` maps a name to its binaries for
/// the architecture.
fn judge(
    deps: &[BuildRequire],
    repos: &[(String, Vec<String>)],
    hits: &HashMap<String, Vec<Hit>>,
    own: &str,
) -> Vec<DepVerdict> {
    deps.iter()
        .map(|dep| {
            let name = match dep {
                BuildRequire::Package(n) => n,
                BuildRequire::Capability(c) => {
                    return DepVerdict {
                        name: c.clone(),
                        state: DepState::Unchecked,
                    };
                }
            };
            let published = hits.get(name).map(Vec::as_slice).unwrap_or_default();
            if published.is_empty() {
                return DepVerdict {
                    name: name.clone(),
                    state: DepState::NotPublished,
                };
            }
            let mut provider = None;
            let mut options = Vec::new();
            for (repo, reach) in repos {
                // The first project on the path chain that publishes it.
                let first = reach
                    .iter()
                    .find_map(|p| published.iter().find(|h| &h.project == p));
                match first {
                    Some(h) => {
                        provider.get_or_insert_with(|| h.clone());
                    }
                    None => {
                        let usable: Vec<Hit> = published
                            .iter()
                            .filter(|h| {
                                h.repository == *repo
                                    && h.project != own
                                    && !h.project.starts_with("home:")
                            })
                            .cloned()
                            .collect();
                        options.push((repo.clone(), usable));
                    }
                }
            }
            let state = if options.is_empty() {
                match provider {
                    Some(provider) => DepState::Reachable { provider },
                    None => DepState::NotPublished,
                }
            } else if options.iter().all(|(_, h)| h.is_empty()) {
                DepState::OnlyUnusable {
                    hits: published.to_vec(),
                }
            } else {
                DepState::Missing { options }
            };
            DepVerdict {
                name: name.clone(),
                state,
            }
        })
        .collect()
}

/// Pick the paths to add so every dependency that can be covered is, with as
/// few projects as possible: repeatedly take the project covering the most
/// still-uncovered `(dependency, repository)` pairs. Ties go to the name that
/// sorts first, so the answer does not depend on hash order. A path points at
/// the repository of the same name in the added project, since that is the
/// only one known to match the branch repository's distribution.
pub fn choose_paths(verdicts: &[DepVerdict]) -> Vec<PathAddition> {
    let mut uncovered: HashSet<(usize, String)> = HashSet::new();
    for (i, v) in verdicts.iter().enumerate() {
        if let DepState::Missing { options } = &v.state {
            for (repo, hits) in options {
                if !hits.is_empty() {
                    uncovered.insert((i, repo.clone()));
                }
            }
        }
    }
    let covers = |project: &str, (i, repo): &(usize, String)| match &verdicts[*i].state {
        DepState::Missing { options } => options
            .iter()
            .any(|(r, hits)| r == repo && hits.iter().any(|h| h.project == project)),
        _ => false,
    };
    let mut chosen = Vec::new();
    while !uncovered.is_empty() {
        let mut score: HashMap<&str, usize> = HashMap::new();
        for v in verdicts {
            if let DepState::Missing { options } = &v.state {
                for h in options.iter().flat_map(|(_, hits)| hits) {
                    score.entry(h.project.as_str()).or_insert(0);
                }
            }
        }
        for (project, n) in score.iter_mut() {
            *n = uncovered.iter().filter(|u| covers(project, u)).count();
        }
        let Some((&best, _)) = score
            .iter()
            .filter(|(_, n)| **n > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        else {
            break;
        };
        let mut repos: Vec<String> = uncovered
            .iter()
            .filter(|u| covers(best, u))
            .map(|(_, r)| r.clone())
            .collect();
        repos.sort();
        repos.dedup();
        for repo in &repos {
            chosen.push(PathAddition {
                repository: repo.clone(),
                project: best.to_string(),
                path_repository: repo.clone(),
            });
        }
        let best = best.to_string();
        uncovered.retain(|u| !covers(&best, u));
    }
    chosen
}

/// The projects the dependencies will be built against that are not distro
/// projects, personal projects, `own` or `exclude`: the places a dependency
/// could itself be updated. Reachable dependencies count by the project the
/// build finds them in; missing ones by the project `added` points at.
pub fn dependency_origins(
    verdicts: &[DepVerdict],
    added: &[PathAddition],
    own: &str,
    exclude: &str,
) -> Vec<(String, String)> {
    let usable =
        |p: &str| !is_distro_project(p) && !p.starts_with("home:") && p != own && p != exclude;
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |h: &Hit| {
        if let (true, Some(pkg)) = (usable(&h.project), &h.package) {
            let pair = (h.project.clone(), pkg.clone());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    };
    for v in verdicts {
        match &v.state {
            DepState::Reachable { provider } => push(provider),
            DepState::Missing { options } => {
                let hit = options
                    .iter()
                    .flat_map(|(_, hits)| hits)
                    .find(|h| added.iter().any(|a| a.project == h.project));
                if let Some(h) = hit {
                    push(h);
                }
            }
            _ => {}
        }
    }
    out
}

/// Reachability checks and path edits for one branch project and
/// architecture, with the OBS answers cached for the run.
pub struct DepResolver {
    obs: Box<dyn Obs>,
    reporter: Arc<dyn Reporter + Send + Sync>,
    branch_project: String,
    arch: String,
    metas: Mutex<HashMap<String, Option<ProjectInfo>>>,
    binaries: Mutex<HashMap<String, Vec<Hit>>>,
    /// Held across a meta read-modify-write, so two edits cannot interleave.
    edit: Mutex<()>,
}

impl DepResolver {
    pub fn new(
        obs: Box<dyn Obs>,
        reporter: Arc<dyn Reporter + Send + Sync>,
        branch_project: &str,
        arch: &str,
    ) -> Self {
        DepResolver {
            obs,
            reporter,
            branch_project: branch_project.to_string(),
            arch: arch.to_string(),
            metas: Mutex::default(),
            binaries: Mutex::default(),
            edit: Mutex::new(()),
        }
    }

    /// The meta of a project other than the branch project, from the cache
    /// when it was read before. Unreadable counts as absent.
    fn path_project_info(&self, project: &str) -> Option<ProjectInfo> {
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

    fn warn(&self, message: String) {
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
    fn branch_info(&self) -> Result<(String, ProjectInfo), String> {
        let meta = self
            .obs
            .project_meta(&self.branch_project)?
            .ok_or_else(|| format!("branch project {} does not exist", self.branch_project))?;
        let info = osc::parse_project_info(&meta);
        Ok((meta, info))
    }

    /// Fetch and cache the binaries of every name not asked about yet.
    fn prefetch(&self, names: &[&str]) -> Result<(), String> {
        let todo: Vec<&str> = {
            let cache = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
            let mut seen = HashSet::new();
            names
                .iter()
                .copied()
                .filter(|n| !cache.contains_key(*n) && seen.insert(*n))
                .collect()
        };
        for chunk in todo.chunks(SEARCH_CHUNK) {
            let found = self.obs.published_binaries(chunk)?;
            let mut cache = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
            for name in chunk {
                let hits: Vec<Hit> = found
                    .iter()
                    .filter(|b| b.name == *name && b.arch == self.arch)
                    .map(Hit::of)
                    .collect();
                cache.insert(name.to_string(), hits);
            }
        }
        Ok(())
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
        let hits = self
            .binaries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Ok(judge(deps, &repos, &hits, &self.branch_project))
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

#[cfg(test)]
mod tests {
    use super::*;
    use ap_core::sink::NullReporter;

    fn meta(name: &str, repos: &[(&str, &[(&str, &str)])]) -> String {
        let mut xml = format!("<project name=\"{name}\">\n");
        for (repo, paths) in repos {
            xml.push_str(&format!("  <repository name=\"{repo}\">\n"));
            for (p, r) in *paths {
                xml.push_str(&format!("    <path project=\"{p}\" repository=\"{r}\"/>\n"));
            }
            xml.push_str("    <arch>x86_64</arch>\n  </repository>\n");
        }
        xml.push_str("</project>\n");
        xml
    }

    fn bin(name: &str, project: &str, repo: &str, package: &str) -> PublishedBinary {
        PublishedBinary {
            name: name.into(),
            project: project.into(),
            repository: repo.into(),
            arch: "x86_64".into(),
            package: Some(package.into()),
            version: Some("1".into()),
        }
    }

    struct Fake {
        metas: Mutex<HashMap<String, String>>,
        binaries: Vec<PublishedBinary>,
        searches: Mutex<usize>,
    }

    impl Obs for Fake {
        fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
            Ok(self.metas.lock().unwrap().get(p).cloned())
        }
        fn set_project_meta(&self, p: &str, xml: &str) -> Result<(), String> {
            self.metas.lock().unwrap().insert(p.into(), xml.into());
            Ok(())
        }
        fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String> {
            *self.searches.lock().unwrap() += 1;
            Ok(self
                .binaries
                .iter()
                .filter(|b| names.contains(&b.name.as_str()))
                .cloned()
                .collect())
        }
    }

    const BRANCH: &str = "home:u:branches:Base:System";

    /// The branch builds against Base:System, which builds against Factory.
    /// `libfoo` is published by devel:languages:c_c++, which nothing reaches.
    fn fixture() -> (Fake, Arc<dyn Reporter + Send + Sync>) {
        let mut metas = HashMap::new();
        metas.insert(
            BRANCH.to_string(),
            meta(
                BRANCH,
                &[
                    (
                        "openSUSE_Tumbleweed",
                        &[("Base:System", "openSUSE_Tumbleweed")],
                    ),
                    ("15.6", &[("Base:System", "15.6")]),
                ],
            ),
        );
        metas.insert(
            "Base:System".into(),
            meta(
                "Base:System",
                &[
                    ("openSUSE_Tumbleweed", &[("openSUSE:Factory", "snapshot")]),
                    ("15.6", &[("openSUSE:Leap:15.6", "standard")]),
                ],
            ),
        );
        metas.insert(
            "openSUSE:Factory".into(),
            meta("openSUSE:Factory", &[("snapshot", &[])]),
        );
        metas.insert(
            "openSUSE:Leap:15.6".into(),
            meta("openSUSE:Leap:15.6", &[("standard", &[])]),
        );
        let binaries = vec![
            bin("cmake", "openSUSE:Factory", "snapshot", "cmake"),
            bin("cmake", "openSUSE:Leap:15.6", "standard", "cmake"),
            bin("cmake", "devel:tools", "openSUSE_Tumbleweed", "cmake"),
            bin(
                "libfoo-devel",
                "devel:languages:c_c++",
                "openSUSE_Tumbleweed",
                "libfoo",
            ),
            bin("libfoo-devel", "devel:languages:c_c++", "15.6", "libfoo"),
            bin(
                "libfoo-devel",
                "home:other:wip",
                "openSUSE_Tumbleweed",
                "libfoo",
            ),
            bin(
                "libbar-devel",
                "devel:languages:c_c++",
                "openSUSE_Tumbleweed",
                "libbar",
            ),
            bin("libbar-devel", "devel:languages:c_c++", "15.6", "libbar"),
            bin(
                "libbar-devel",
                "devel:other",
                "openSUSE_Tumbleweed",
                "libbar",
            ),
            bin("libbar-devel", "devel:other", "15.6", "libbar"),
            bin("onlyhome", "home:x", "openSUSE_Tumbleweed", "onlyhome"),
            bin("wrongrepo", "devel:z", "Other_Name", "wrongrepo"),
        ];
        (
            Fake {
                metas: Mutex::new(metas),
                binaries,
                searches: Mutex::new(0),
            },
            Arc::new(NullReporter),
        )
    }

    fn pkg(n: &str) -> BuildRequire {
        BuildRequire::Package(n.into())
    }

    fn resolver() -> (DepResolver, Arc<Fake>) {
        struct Shared(Arc<Fake>);
        impl Obs for Shared {
            fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
                self.0.project_meta(p)
            }
            fn set_project_meta(&self, p: &str, x: &str) -> Result<(), String> {
                self.0.set_project_meta(p, x)
            }
            fn published_binaries(&self, n: &[&str]) -> Result<Vec<PublishedBinary>, String> {
                self.0.published_binaries(n)
            }
        }
        let (fake, reporter) = fixture();
        let fake = Arc::new(fake);
        (
            DepResolver::new(Box::new(Shared(fake.clone())), reporter, BRANCH, "x86_64"),
            fake,
        )
    }

    fn state<'a>(v: &'a [DepVerdict], name: &str) -> &'a DepState {
        &v.iter().find(|v| v.name == name).unwrap().state
    }

    #[test]
    fn a_dependency_a_path_chain_publishes_is_reachable_through_the_paths_of_paths() {
        let (r, _) = resolver();
        let v = r.check(&[pkg("cmake")]).unwrap();
        // Found in Factory (via Base:System) and Leap, not devel:tools, which
        // nothing reaches.
        match state(&v, "cmake") {
            DepState::Reachable { provider } => {
                assert_eq!(provider.project, "openSUSE:Factory");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_dependency_in_an_unreached_project_is_missing_with_per_repository_options() {
        let (r, _) = resolver();
        let v = r.check(&[pkg("libfoo-devel")]).unwrap();
        let DepState::Missing { options } = state(&v, "libfoo-devel") else {
            panic!("{v:?}");
        };
        let repos: Vec<&str> = options.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(repos, ["openSUSE_Tumbleweed", "15.6"]);
        // Another user's home project is never offered.
        assert!(
            options[0]
                .1
                .iter()
                .all(|h| h.project == "devel:languages:c_c++"),
            "{options:?}"
        );
    }

    #[test]
    fn names_nobody_can_usefully_provide_are_classified_as_such() {
        let (r, _) = resolver();
        let v = r
            .check(&[
                pkg("onlyhome"),
                pkg("wrongrepo"),
                pkg("nothing-publishes-this"),
                BuildRequire::Capability("pkgconfig(x)".into()),
            ])
            .unwrap();
        assert!(matches!(
            state(&v, "onlyhome"),
            DepState::OnlyUnusable { .. }
        ));
        assert!(matches!(
            state(&v, "wrongrepo"),
            DepState::OnlyUnusable { .. }
        ));
        assert_eq!(state(&v, "nothing-publishes-this"), &DepState::NotPublished);
        assert_eq!(state(&v, "pkgconfig(x)"), &DepState::Unchecked);
        assert!(choose_paths(&v).is_empty());
    }

    #[test]
    fn the_project_covering_most_dependencies_is_chosen_first_and_once_per_repository() {
        let (r, _) = resolver();
        let v = r
            .check(&[pkg("libfoo-devel"), pkg("libbar-devel")])
            .unwrap();
        let adds = choose_paths(&v);
        // devel:languages:c_c++ covers both in both repositories (libbar also
        // has devel:other, which would be a second project for nothing).
        assert_eq!(
            adds,
            vec![
                PathAddition {
                    repository: "15.6".into(),
                    project: "devel:languages:c_c++".into(),
                    path_repository: "15.6".into()
                },
                PathAddition {
                    repository: "openSUSE_Tumbleweed".into(),
                    project: "devel:languages:c_c++".into(),
                    path_repository: "openSUSE_Tumbleweed".into()
                },
            ]
        );
    }

    #[test]
    fn adding_the_chosen_paths_edits_the_meta_once_and_is_idempotent() {
        let (r, fake) = resolver();
        let v = r.check(&[pkg("libfoo-devel")]).unwrap();
        let adds = choose_paths(&v);
        let made = r.add_paths(&adds).unwrap();
        assert_eq!(made.len(), 2);
        let info = osc::parse_project_info(&fake.metas.lock().unwrap()[BRANCH]);
        for repo in &info.repositories {
            assert_eq!(
                repo.paths.last().unwrap().0,
                "devel:languages:c_c++",
                "{repo:?}"
            );
            assert_eq!(repo.paths.len(), 2);
        }
        // Reachable now, so nothing more to add; and adding again is a no-op.
        let v = r.check(&[pkg("libfoo-devel")]).unwrap();
        assert!(matches!(
            state(&v, "libfoo-devel"),
            DepState::Reachable { .. }
        ));
        assert!(r.add_paths(&adds).unwrap().is_empty());
    }

    #[test]
    fn a_name_is_searched_for_once_per_run() {
        let (r, fake) = resolver();
        r.check(&[pkg("cmake"), pkg("libfoo-devel")]).unwrap();
        r.check(&[pkg("cmake")]).unwrap();
        r.check(&[pkg("libfoo-devel"), pkg("cmake")]).unwrap();
        assert_eq!(*fake.searches.lock().unwrap(), 1);
    }

    #[test]
    fn dependencies_are_sent_to_the_search_in_chunks() {
        let (r, fake) = resolver();
        let deps: Vec<BuildRequire> = (0..45).map(|i| pkg(&format!("p{i}"))).collect();
        r.check(&deps).unwrap();
        assert_eq!(*fake.searches.lock().unwrap(), 3);
    }

    #[test]
    fn covering_a_missing_dependency_adds_a_path_instead_of_building_it() {
        let (r, fake) = resolver();
        let note = r
            .cover_dependency(&["libfoo-devel".into(), "python-libfoo-devel".into()])
            .unwrap()
            .expect("covered");
        assert!(note.contains("devel:languages:c_c++"), "{note}");
        let info = osc::parse_project_info(&fake.metas.lock().unwrap()[BRANCH]);
        assert!(
            info.repositories[0]
                .paths
                .iter()
                .any(|p| p.0 == "devel:languages:c_c++")
        );
        // A second report of it finds it reachable and edits nothing.
        let before = fake.metas.lock().unwrap()[BRANCH].clone();
        let note = r
            .cover_dependency(&["libfoo-devel".into()])
            .unwrap()
            .unwrap();
        assert!(note.contains("already builds against"), "{note}");
        assert_eq!(fake.metas.lock().unwrap()[BRANCH], before);
    }

    #[test]
    fn a_dependency_nothing_publishes_is_left_for_the_pipeline_to_build() {
        let (r, fake) = resolver();
        let before = fake.metas.lock().unwrap()[BRANCH].clone();
        assert_eq!(
            r.cover_dependency(&["python-nope".into(), "nope".into()])
                .unwrap(),
            None
        );
        assert_eq!(r.cover_dependency(&["onlyhome".into()]).unwrap(), None);
        assert_eq!(fake.metas.lock().unwrap()[BRANCH], before);
    }

    #[test]
    fn a_path_cycle_does_not_hang_the_walk() {
        let (r, fake) = resolver();
        fake.set_project_meta(
            "openSUSE:Factory",
            &meta(
                "openSUSE:Factory",
                &[("snapshot", &[("Base:System", "openSUSE_Tumbleweed")])],
            ),
        )
        .unwrap();
        fake.set_project_meta(
            "Base:System",
            &meta(
                "Base:System",
                &[
                    ("openSUSE_Tumbleweed", &[("openSUSE:Factory", "snapshot")]),
                    ("15.6", &[("openSUSE:Leap:15.6", "standard")]),
                ],
            ),
        )
        .unwrap();
        let v = r.check(&[pkg("cmake")]).unwrap();
        assert!(matches!(state(&v, "cmake"), DepState::Reachable { .. }));
    }

    #[test]
    fn only_non_distro_non_personal_projects_are_candidates_for_updating_a_dependency() {
        let (r, _) = resolver();
        let v = r
            .check(&[pkg("cmake"), pkg("libfoo-devel"), pkg("onlyhome")])
            .unwrap();
        let adds = choose_paths(&v);
        let origins = dependency_origins(&v, &adds, BRANCH, "Base:System");
        // cmake is Factory's (a distro project), onlyhome is personal.
        assert_eq!(
            origins,
            vec![("devel:languages:c_c++".to_string(), "libfoo".to_string())]
        );
        // The project being updated from is not an origin of its own dependency.
        assert!(dependency_origins(&v, &adds, BRANCH, "devel:languages:c_c++").is_empty());
    }

    #[test]
    fn a_branch_without_a_repository_for_the_arch_is_an_error() {
        let (fake, reporter) = fixture();
        let r = DepResolver::new(Box::new(fake), reporter, BRANCH, "aarch64");
        assert!(r.check(&[pkg("cmake")]).unwrap_err().contains("aarch64"));
    }

    #[test]
    fn distro_projects_are_recognised() {
        assert!(is_distro_project("openSUSE:Factory"));
        assert!(is_distro_project("SUSE:SLE-15-SP6:GA"));
        assert!(!is_distro_project("devel:languages:python"));
        assert!(!is_distro_project("Base:System"));
    }
}
