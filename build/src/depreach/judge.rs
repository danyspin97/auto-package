use super::*;

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
    pub(super) fn of(b: &PublishedBinary) -> Self {
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

/// The `(project, repository)` pairs `repository` of a project builds against.
fn repository_pairs(info: &ProjectInfo, repository: &str) -> Vec<osc::PathPair> {
    info.repositories
        .iter()
        .filter(|r| r.name == repository)
        .flat_map(|r| r.paths.clone())
        .collect()
}

/// The projects reachable from `repository` of a project, the project itself
/// first, then in OBS path order (see [`osc::walk_path_pairs`]). `info_of`
/// yields the meta of a path project, or `None` for one that cannot be read:
/// the project is still listed, but its paths are not followed.
pub(super) fn reachable_projects(
    own: &str,
    own_info: &ProjectInfo,
    repository: &str,
    info_of: &mut dyn FnMut(&str) -> Option<ProjectInfo>,
) -> Vec<String> {
    let found = osc::walk_path_pairs(
        own,
        repository_pairs(own_info, repository),
        usize::MAX,
        osc::MAX_BUILD_PATH_NODES,
        |project, repo, _, _| {
            let paths = info_of(project)
                .map(|info| repository_pairs(&info, repo))
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>(Some(((), paths)))
        },
    )
    .unwrap_or_default();
    let mut order = vec![own.to_string()];
    for (project, _, ()) in found {
        if !order.contains(&project) {
            order.push(project);
        }
    }
    order
}

/// Classify each dependency against the projects `reach` (per target
/// repository) and what OBS publishes. `hits` maps a name to its binaries for
/// the architecture.
pub(super) fn judge(
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
            // A name the search cannot carry is never looked up, so nothing is
            // known about it: unchecked, not "published nowhere".
            if !osc::is_searchable_binary_name(name) {
                return DepVerdict {
                    name: name.clone(),
                    state: DepState::Unchecked,
                };
            }
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
