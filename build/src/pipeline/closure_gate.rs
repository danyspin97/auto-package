//! The resolvability gate of a dependency closure: which of its nodes are
//! already resolved, and the frontier of what is buildable next.

use super::*;

/// How a node of the closure is resolved before the frontier is chosen.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Resolution {
    No,
    /// Present in a linked/path project, or a system-tool alias
    /// ([`SYSTEM_TOOL_ALIASES`]) — the distro already has it.
    Distro,
    /// Present in this project's own OBS listing, or already built in
    /// the local workspace.
    Built,
}

/// Whether the requested root is still to be built under `--force`.
///
/// `--force` asks for one rebuild of the root. Until this invocation has built
/// it (`finished_here`, see [`FinishedRoots`]) the root is unresolved, whatever
/// OBS and the state say, so the frontier selects it. Once it has, it is
/// resolved like any other node: otherwise every round selects and rebuilds it
/// again, and the run never ends.
pub(super) fn forced_root_pending(force: bool, is_requested: bool, finished_here: bool) -> bool {
    force && is_requested && !finished_here
}

/// What the resolution of one node of the closure depends on, as plain values.
pub(super) struct NodeFacts {
    pub(super) force: bool,
    pub(super) is_requested: bool,
    /// This invocation has finished the requested root.
    pub(super) root_finished: bool,
    /// The process settled the node: as a dependency, or as some invocation's root.
    pub(super) settled: bool,
    pub(super) system_alias: bool,
    /// Present in a linked/path project.
    pub(super) linked: bool,
}

/// The facts [`node_resolution`] needs about `node`. `settled` is the
/// process-wide [`is_settled`] answer for it; `linked_fuzzy` holds the
/// `project_key`s of the linked projects' listings.
pub(super) fn node_facts(
    node: &ap_ecosystem::pip::Node,
    force: bool,
    finished: &FinishedRoots,
    settled: bool,
    linked_fuzzy: &std::collections::HashSet<String>,
) -> NodeFacts {
    let rpm = osc::rpm_name(&node.name);
    NodeFacts {
        force,
        is_requested: node.is_requested,
        root_finished: finished.contains(&rpm),
        settled,
        system_alias: is_system_tool_alias(&node.name),
        linked: linked_fuzzy.contains(&osc::project_key(&rpm)),
    }
}

/// How a node is resolved. `own` is the node's status in this project's own
/// OBS listing (`None`: not listed there; `Some(ok)`: listed, and whether it
/// counts as built). `built_in_state` reads whether the local state records it
/// built. Both are only evaluated when the facts before them do not decide.
pub(super) fn node_resolution(
    facts: &NodeFacts,
    own: impl FnOnce() -> Option<bool>,
    built_in_state: impl FnOnce() -> bool,
) -> Resolution {
    // `--force` on the root: see `forced_root_pending`.
    if forced_root_pending(facts.force, facts.is_requested, facts.root_finished) {
        return Resolution::No;
    }
    // Some PyPI packages exist purely to redistribute a binary that
    // openSUSE already ships as its own identically-named system package
    // (`ninja`, pulled in by build backends that shell out to it), so they
    // are never selected as a build target — see `SYSTEM_TOOL_ALIASES`.
    if facts.system_alias {
        return Resolution::Distro;
    }
    if facts.settled {
        return Resolution::Built;
    }
    match own() {
        Some(true) => return Resolution::Built,
        Some(false) => return Resolution::No,
        None => {}
    }
    if facts.linked {
        return Resolution::Distro;
    }
    if built_in_state() {
        Resolution::Built
    } else {
        Resolution::No
    }
}

/// The frontier of a closure: the unresolved nodes whose declared deps inside
/// the closure are all resolved, leaving out the packages given up on (by
/// `failed_keys`, a set of `fuzzy_key`s). `root_idx` is the node that gets
/// [`SelectionReason::RequestedRoot`]; the others are first unbuilt deps.
pub(super) fn frontier_of(
    nodes: &[ap_ecosystem::pip::Node],
    resolvable: &[bool],
    root_idx: usize,
    failed_keys: &std::collections::HashSet<String>,
) -> Vec<TargetInfo> {
    // Build a normalized name → index map for dep lookups.
    let name_to_idx: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (normalize_pkg_name(&n.name), i))
        .collect();
    let mut frontier: Vec<TargetInfo> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if resolvable[i] {
            continue;
        }
        // Given up on this one already — leave it (and anything that needs
        // it) out of the frontier instead of re-selecting it forever.
        if failed_keys.contains(&osc::fuzzy_key(&osc::rpm_name(&node.name))) {
            continue;
        }

        let deps_satisfied = dep_names_from_requires_dist(&node.requires_dist)
            .into_iter()
            .all(|dep| {
                name_to_idx
                    .get(&dep)
                    .map(|&j| resolvable[j])
                    .unwrap_or(true) // not in closure → provided by system
            });

        if !deps_satisfied {
            continue;
        }

        let reason = if i == root_idx {
            SelectionReason::RequestedRoot
        } else {
            SelectionReason::FirstUnbuiltDependency
        };

        frontier.push(TargetInfo {
            name: node.name.clone(),
            version: node.version.clone(),
            reason,
            blocked_by: vec![],
        });
    }
    frontier
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, requires: &[&str], requested: bool) -> ap_ecosystem::pip::Node {
        ap_ecosystem::pip::Node {
            name: name.to_string(),
            version: "1.0".to_string(),
            sha256: String::new(),
            requires_dist: requires.iter().map(|r| r.to_string()).collect(),
            requires_python: None,
            is_requested: requested,
            is_yanked: false,
        }
    }

    /// The closure of `python-pywayland` as the log of the rebuild loop shows
    /// it: pycparser and cffi are built, and pywayland (the root) is the one
    /// node left.
    fn pywayland_closure() -> Vec<ap_ecosystem::pip::Node> {
        vec![
            node("pycparser", &[], false),
            node("cffi", &["pycparser"], false),
            node("pywayland", &["cffi"], true),
        ]
    }

    /// The resolvability of each node as `select_target` computes it. Every
    /// node but the root is in the local state; `root_settled` is the process
    /// answer for the root, `finished` the roots this invocation finished.
    fn resolvable_of(
        nodes: &[ap_ecosystem::pip::Node],
        force: bool,
        finished: &FinishedRoots,
        root_settled: bool,
    ) -> Vec<bool> {
        let linked = std::collections::HashSet::new();
        nodes
            .iter()
            .map(|n| {
                let settled = n.is_requested && root_settled;
                let facts = node_facts(n, force, finished, settled, &linked);
                node_resolution(&facts, || None, || !n.is_requested) != Resolution::No
            })
            .collect()
    }

    fn built(rpm: &str) -> PipelineResult {
        PipelineResult::Built {
            rpm_name: rpm.to_string(),
        }
    }

    #[test]
    fn a_forced_root_is_pending_only_until_this_invocation_has_built_it() {
        assert!(forced_root_pending(true, true, false));
        assert!(!forced_root_pending(true, true, true));
        assert!(!forced_root_pending(false, true, false));
        assert!(!forced_root_pending(true, false, false));
    }

    #[test]
    fn a_forced_root_finished_in_this_invocation_leaves_the_frontier_empty() {
        let nodes = pywayland_closure();
        let failed = std::collections::HashSet::new();
        let mut finished = FinishedRoots::default();
        // Before the build: the root is the frontier, as it must be.
        let before = resolvable_of(&nodes, true, &finished, false);
        assert_eq!(before, [true, true, false]);
        let frontier = frontier_of(&nodes, &before, 2, &failed);
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].name, "pywayland");
        // After the build this invocation has finished it: the closure is
        // resolved and nothing is selected, so the loop ends.
        finished.note(
            "python-pywayland",
            "python-pywayland",
            &built("python-pywayland"),
        );
        let after = resolvable_of(&nodes, true, &finished, true);
        assert!(after.iter().all(|&r| r));
        assert!(frontier_of(&nodes, &after, 2, &failed).is_empty());
    }

    #[test]
    fn a_forced_root_settled_only_as_a_dependency_elsewhere_is_rebuilt() {
        let nodes = pywayland_closure();
        let failed = std::collections::HashSet::new();
        // Another invocation or a dependency closure settled the root, but
        // this invocation has not finished it: it is still selected.
        let finished = FinishedRoots::default();
        let resolvable = resolvable_of(&nodes, true, &finished, true);
        assert_eq!(resolvable, [true, true, false]);
        let frontier = frontier_of(&nodes, &resolvable, 2, &failed);
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].name, "pywayland");
        assert!(matches!(frontier[0].reason, SelectionReason::RequestedRoot));
    }

    #[test]
    fn a_non_forced_root_settled_anywhere_stays_resolved() {
        let nodes = pywayland_closure();
        let finished = FinishedRoots::default();
        let resolvable = resolvable_of(&nodes, false, &finished, true);
        assert_eq!(resolvable, [true, true, true]);
    }

    #[test]
    fn a_settled_dependency_is_resolved_even_when_forced() {
        let facts = NodeFacts {
            force: true,
            is_requested: false,
            root_finished: false,
            settled: true,
            system_alias: false,
            linked: false,
        };
        assert_eq!(
            node_resolution(&facts, || None, || false),
            Resolution::Built
        );
    }

    #[test]
    fn a_forced_root_is_not_resolved_by_its_own_listing_or_state() {
        let facts = NodeFacts {
            force: true,
            is_requested: true,
            root_finished: false,
            settled: false,
            system_alias: false,
            linked: false,
        };
        assert_eq!(
            node_resolution(&facts, || Some(true), || true),
            Resolution::No
        );
    }

    #[test]
    fn an_unforced_node_resolves_by_its_listing_link_or_state() {
        let mut facts = NodeFacts {
            force: false,
            is_requested: true,
            root_finished: false,
            settled: false,
            system_alias: false,
            linked: false,
        };
        assert_eq!(
            node_resolution(&facts, || Some(true), || false),
            Resolution::Built
        );
        assert_eq!(
            node_resolution(&facts, || Some(false), || true),
            Resolution::No
        );
        assert_eq!(node_resolution(&facts, || None, || true), Resolution::Built);
        assert_eq!(node_resolution(&facts, || None, || false), Resolution::No);
        facts.linked = true;
        assert_eq!(
            node_resolution(&facts, || None, || false),
            Resolution::Distro
        );
        facts.system_alias = true;
        assert_eq!(
            node_resolution(&facts, || Some(false), || false),
            Resolution::Distro
        );
    }

    #[test]
    fn only_a_root_this_invocation_built_counts_as_finished() {
        let mut finished = FinishedRoots::default();
        finished.note("python-pywayland", "python-cffi", &built("python-cffi"));
        finished.note(
            "python-pywayland",
            "python-pywayland",
            &PipelineResult::Failed {
                rpm_name: "python-pywayland".to_string(),
                reason: "boom".to_string(),
                kind: FailureKind::Other,
                remedy: None,
            },
        );
        assert!(!finished.contains("python-pywayland"));
        assert!(!finished.contains("python-cffi"));
        finished.note(
            "python-pywayland",
            "python-PyWayland",
            &PipelineResult::AlreadyBuilt {
                rpm_name: "python-PyWayland".to_string(),
            },
        );
        assert!(finished.contains("python-pywayland"));
    }

    #[test]
    fn a_given_up_node_stays_out_of_the_frontier() {
        let nodes = pywayland_closure();
        let mut failed = std::collections::HashSet::new();
        failed.insert(osc::fuzzy_key("python-pywayland"));
        assert!(frontier_of(&nodes, &[true, true, false], 2, &failed).is_empty());
    }
}
