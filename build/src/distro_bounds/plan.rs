use super::*;

/// What the check found.
#[derive(Debug, Default)]
pub struct Plan {
    pub conflicts: Vec<BoundConflict>,
    /// Indices of distro nodes to build here after all (their distro version
    /// is too old for a dependent that is built here).
    pub rebuild: Vec<usize>,
    /// Other findings worth a line in the run log.
    pub notes: Vec<String>,
}

/// The nodes `distro` marks that a node in `needs_build` declares a bound on:
/// the ones whose shipped version is worth looking up.
pub fn candidates(nodes: &[Node], distro: &[bool], needs_build: &[bool]) -> Vec<usize> {
    let by_name = index_by_name(nodes);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if !needs_build[i] {
            continue;
        }
        for (j, _) in bounded_deps(node, &by_name) {
            if distro[j] && seen.insert(j) {
                out.push(j);
            }
        }
    }
    out
}

pub(super) fn index_by_name(nodes: &[Node]) -> HashMap<String, usize> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (fuzzy_key(&n.name), i))
        .collect()
}

/// The closure nodes `node` declares a version bound on, with the bound text.
pub(super) fn bounded_deps(node: &Node, by_name: &HashMap<String, usize>) -> Vec<(usize, String)> {
    node.requires_dist
        .iter()
        .filter_map(|line| {
            let (name, spec, marker) = parse_requirement(line)?;
            if spec.is_empty() || !marker.as_deref().is_none_or(marker_applies) {
                return None;
            }
            // A CUDA component's version is NVIDIA's, not the wheel's: a bound
            // on the wheel (`nvidia-cublas>=13.1`) says nothing about it.
            if crate::redist::component(&name).is_some() {
                return None;
            }
            Some((*by_name.get(&fuzzy_key(&name))?, spec))
        })
        .collect()
}

/// Check every bound a node that is built here declares on a node that comes
/// from the distro. `distro[i]` is true for a node the linked project supplies,
/// `needs_build[i]` for one that still has to be built here (the requested
/// root included). `distro_version(i)` is the version the linked project ships
/// for node `i`, or `None` when it cannot be told: a dependency whose version
/// is unknown, or whose bound cannot be parsed, is never reported.
///
/// A distro node that must be rebuilt here becomes a node that is built here,
/// so its own bounds are checked too, until nothing changes.
pub fn plan(
    nodes: &[Node],
    distro: &[bool],
    needs_build: &[bool],
    mut distro_version: impl FnMut(usize) -> Option<String>,
) -> Plan {
    let by_name = index_by_name(nodes);
    let mut distro = distro.to_vec();
    let mut needs_build = needs_build.to_vec();
    let mut versions: HashMap<usize, Option<String>> = HashMap::new();
    let mut plan = Plan::default();
    let mut reported: HashSet<(usize, usize)> = HashSet::new();
    loop {
        let mut changed = false;
        for i in 0..nodes.len() {
            if !needs_build[i] {
                continue;
            }
            for (j, spec) in bounded_deps(&nodes[i], &by_name) {
                if !distro[j] || reported.contains(&(i, j)) {
                    continue;
                }
                let Some(shipped) = versions
                    .entry(j)
                    .or_insert_with(|| distro_version(j))
                    .clone()
                else {
                    continue;
                };
                let (Some(clauses), Some(v)) = (parse_specifier(&spec), parse_version(&shipped))
                else {
                    continue;
                };
                let Some(kind) = violation(&clauses, &v) else {
                    continue;
                };
                reported.insert((i, j));
                plan.conflicts.push(BoundConflict {
                    dependency: nodes[j].name.clone(),
                    distro_version: shipped,
                    resolved_version: nodes[j].version.clone(),
                    dependent: nodes[i].name.clone(),
                    specifier: spec,
                    kind,
                });
                if kind == Kind::DistroTooOld {
                    distro[j] = false;
                    needs_build[j] = true;
                    plan.rebuild.push(j);
                    changed = true;
                }
            }
        }
        if !changed {
            return plan;
        }
    }
}

/// A warning for the requested package when the linked project ships an older
/// release than the one pip resolved: it counts as resolved by presence, so
/// without this the run would end having built everything but the package that
/// was asked for, with nothing saying why.
pub fn stale_root_message(node: &Node, shipped: &str, project: &str) -> Option<String> {
    use crate::version::{Scheme, cmp};
    (cmp(shipped, &node.version, Scheme::Pep440)? == std::cmp::Ordering::Less).then(|| {
        format!(
            "{} {shipped} (shipped by a project {project} links to) is older than the {} that was resolved, and counts \
             as available: nothing builds it here (pass --force to branch it into this project and update it to {})",
            node.name, node.version, node.version
        )
    })
}

/// `f(i)` for each of `indices`, `jobs` at a time, keeping the answers.
pub fn fetch_parallel(
    indices: &[usize],
    jobs: usize,
    f: impl Fn(usize) -> Option<String> + Sync,
) -> HashMap<usize, String> {
    let next = AtomicUsize::new(0);
    let found = std::sync::Mutex::new(HashMap::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1).min(indices.len()) {
            scope.spawn(|| {
                while let Some(&i) = indices.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(v) = f(i) {
                        found.lock().unwrap_or_else(|e| e.into_inner()).insert(i, v);
                    }
                }
            });
        }
    });
    found.into_inner().unwrap_or_else(|e| e.into_inner())
}
