//! Selecting the target of a run: its version conflicts and the selection itself.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::blocked::*;
use crate::pipeline::gen_spec::*;
use crate::pipeline::linked::*;
use crate::pipeline::obs_names::*;
use crate::pipeline::slots::*;

/// Warnings for the packages `root`'s closure needs at a version another root
/// built into the same `project` resolved differently (open-webui pins
/// `mcp==1.27.2`, vllm needs `mcp>=2`). A project holds one release of a
/// package, so the roots would rebuild it over each other in turn and one of
/// them would end with a requirement nothing satisfies. Each conflict is
/// reported once.
pub(super) fn version_conflicts(
    project: &str,
    root: &str,
    nodes: &[ap_ecosystem::pip::Node],
) -> Vec<String> {
    let mut guard = claims().lock().unwrap_or_else(|e| e.into_inner());
    let (claimed, warned) = &mut *guard;
    let mut out = Vec::new();
    for node in nodes {
        let key = osc::fuzzy_key(&node.name);
        let holders = claimed
            .entry((project.to_string(), key.clone()))
            .or_default();
        holders.retain(|(r, _)| r != root);
        for (other, version) in holders.iter() {
            if *version == node.version {
                continue;
            }
            let id = format!(
                "{project}\0{key}\0{other}\0{root}\0{version}\0{}",
                node.version
            );
            if !warned.insert(id) {
                continue;
            }
            let requirers: Vec<String> = nodes
                .iter()
                .flat_map(|n| {
                    n.requires_dist
                        .iter()
                        .filter(move |r| {
                            let name_end = r
                                .find(|c: char| {
                                    !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                                })
                                .unwrap_or(r.len());
                            osc::fuzzy_key(&r[..name_end]) == osc::fuzzy_key(&node.name)
                                && r.contains("==")
                        })
                        .map(move |r| {
                            format!(
                                "{} requires {}",
                                n.name,
                                r.split(';').next().unwrap_or(r).trim()
                            )
                        })
                })
                .collect();
            out.push(format!(
                "{name} resolves to {version} for {other} but to {mine} for {root}{pins}: {project} holds one \
                 release of it, so the two roots would rebuild it over each other and one of them would \
                 be left with a requirement nothing satisfies. Build them in separate OBS projects \
                 (--project), or relax the pin that holds {root} back.",
                name = node.name,
                mine = node.version,
                pins = if requirers.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", requirers.join("; "))
                },
            ));
        }
        holders.push((root.to_string(), node.version.clone()));
    }
    out
}

/// Whether the local workspace state records the node `n` (RPM name `rpm`) as
/// built at its resolved version.
fn built_in_state(config: &PipelineConfig, n: &ap_ecosystem::pip::Node, rpm: &str) -> bool {
    let ws_name = ap_core::run::sanitize_name(rpm);
    let state_path = config.state_root.join(ws_name).join("state.json");
    match std::fs::read_to_string(&state_path) {
        Ok(s) => match serde_json::from_str::<PkgState>(&s) {
            // A dependency `--local` built here is available to a local
            // build; OBS never has it, so a normal run does not count it.
            Ok(st) => {
                st.is_built(&n.version)
                    || (osc::local_build().is_some() && st.is_built_locally(&n.version))
            }
            Err(e) => {
                ap_core::diag!(
                    "  {} is unreadable ({e}); treating {} as not built",
                    state_path.display(),
                    n.name
                );
                false
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            ap_core::diag!(
                "  cannot read {} ({e}); treating {} as not built",
                state_path.display(),
                n.name
            );
            false
        }
    }
}

/// Resolve the dependency closure and return all currently buildable targets
/// (the "frontier" — nodes whose declared deps in the closure are all resolved).
///
/// Uses `pip install --dry-run` once to get the full runtime closure. A node
/// is "resolvable" if it is already built locally (state.json) or present in
/// the OBS project. A node is in the frontier if it is not yet resolvable and
/// every package it declares in `requires_dist` that also appears in the
/// closure is already resolvable.
///
/// `pinned_version` is the user's `--version`; it constrains the resolve so a
/// pin actually pins rather than being overwritten by whatever pip picks.
///
/// `permanently_failed` holds RPM names the caller has given up retrying
/// (see [`crate::pipeline`] retry-count tracking in the CLI build loop) —
/// they and anything depending on them are excluded from the frontier so a
/// package that can never succeed doesn't get re-selected forever.
///
/// `force` tells the gate the user asked explicitly to rebuild (via `--force`,
/// or by invoking `update`, which always passes it): the root (requested)
/// node is then never treated as resolvable, even if it is already green in
/// OBS and locally. Without this, `--force` is unenforceable from here —
/// `run_package`'s own force check only fires if the package reaches it in the
/// first place, and `select_target` would otherwise skip it before it ever
/// does. Under `force` the root counts as built only once `finished_roots`
/// (the caller's own invocation) has finished it.
///
/// Returns `Ok(vec![])` when every package in the closure is already
/// resolvable, or when nothing further can be built because everything left
/// is blocked by a `permanently_failed` package.
#[allow(clippy::too_many_arguments)]
pub fn select_target(
    name: &str,
    explicit_target: Option<&str>,
    // Pin the root package to this version, if the user asked for one.
    pinned_version: Option<&str>,
    config: &PipelineConfig,
    permanently_failed: &std::collections::HashSet<String>,
    force: bool,
    finished_roots: &FinishedRoots,
    reporter: &dyn Reporter,
) -> Result<Vec<TargetInfo>, String> {
    reporter.report(&Event::ResolveStarted {
        root: name.to_string(),
    });
    warn_unsandboxed_resolve_once(reporter);

    // Kick off the package availability fetch concurrently with pip resolve.
    // Checked separately, not merged: whether a build-status check applies
    // depends on which side found the package (see the `resolvable` comment
    // below).
    // A failed lookup stops the resolve: an empty answer would make every
    // package look absent (and so get rebuilt) or present (and so be skipped).
    let obs_project = config.project.clone();
    let (nodes, own_packages, linked_packages, build_statuses) = std::thread::scope(|scope| {
        let own_handle =
            scope.spawn(|| osc::try_list_project_packages(&obs_project, Emit::run(reporter)));
        let linked_handle = scope.spawn(|| {
            let mut linked = std::collections::HashSet::new();
            let paths = osc::get_project_repo_paths(&obs_project, Emit::run(reporter))
                .map_err(|e| format!("could not read build paths of {obs_project}: {e}"))?;
            for (path_project, _repo) in paths {
                let listed = osc::try_list_project_packages(&path_project, Emit::run(reporter))?;
                linked.extend(listed.iter().cloned());
            }
            Ok::<_, String>(linked)
        });
        let statuses_handle = scope.spawn(|| {
            osc::get_project_build_statuses(&obs_project, &config.distro, &config.arch)
                .map_err(|e| format!("could not read build status of {obs_project}: {e}"))
        });
        let nodes = ap_ecosystem::pip::resolve(name, pinned_version);
        // Join the fetches (likely already done while pip was resolving).
        (
            nodes,
            join_lookup(own_handle, "own"),
            join_lookup(linked_handle, "linked"),
            join_lookup(statuses_handle, "statuses"),
        )
    });
    let mut nodes = nodes.map_err(|e| format!("resolve failed for {name}: {e}"))?;
    // `--nvidia-redist`: a CUDA component is built at NVIDIA's version of it,
    // not at the wheel's.
    crate::redist::apply_manifest_versions(&mut nodes, &crate::redist::Network)?;
    // The library packages require their release's filesystem package: queue it.
    crate::redist::add_filesystem_nodes(&mut nodes, &crate::redist::Network)?;
    let own_packages = own_packages??;
    let linked_packages = linked_packages??;
    let build_statuses = build_statuses??;

    let total = nodes.len();
    for message in version_conflicts(&config.project, name, &nodes) {
        reporter.report(&Event::Warning {
            step: None,
            message,
        });
    }

    // A node already present in the user's own project is resolvable unless
    // OBS reports its own build as actually failing there, in which case it
    // stays unresolved so the frontier picks it back up and `run_package`'s
    // existing build-log-driven fix path can retry it — instead of a broken
    // OBS package silently blocking everything above it forever.
    //
    // A node present only in a linked/build project (e.g. openSUSE:Factory,
    // reached via `osc meta prj`'s `<path>` entries) is resolvable on mere
    // presence, with no build-status check: we don't own or rebuild those
    // packages, so there is nothing to retry, and querying that project's
    // build status per its own repo/arch is unreliable (naming and listing
    // quirks in a project we don't control) — requiring it would cause
    // already-available packages like `python-setuptools` to be queued for
    // packaging again. Presence in the linked project's sources is exactly
    // the signal OBS itself uses to consider a package "available".
    //
    // It's also resolvable if its local state.json records a prior
    // "success" outcome. (`wheel-only` state is a terminal FAILURE marker,
    // not a success — see `PkgState::is_built`'s doc comment — so it does
    // not count here. A wheel-only package stays unresolved and
    // re-selected until it either becomes genuinely buildable from source
    // or the top-level retry gives up on it.)
    // `osc::rpm_name` is only a guess — openSUSE has no single deterministic
    // rule for how a PyPI name's separators map to the RPM name (see its doc
    // comment: `pydantic_core` is packaged as `python-pydantic-core`, but
    // `typing_extensions` keeps its underscore). Exact string equality against
    // the real project listings would make already-available packages like
    // `python-pydantic-core` look unresolvable and get queued as duplicates.
    // Index both listings by a
    // separator-blind `fuzzy_key` instead, and use the *real* listed name
    // (not the guess) for the build-status lookup once a match is found.
    // Several listed spellings can share a key (`python-APScheduler` and
    // `python-apscheduler` are both real OBS packages): all are kept, and the
    // choice among them is `preferred_own_spelling`, not the iteration order.
    let mut own_fuzzy: std::collections::HashMap<String, Vec<&String>> =
        std::collections::HashMap::new();
    for p in own_packages.iter() {
        own_fuzzy.entry(osc::fuzzy_key(p)).or_default().push(p);
    }
    let linked_fuzzy: std::collections::HashSet<String> =
        linked_packages.iter().map(|p| osc::fuzzy_key(p)).collect();

    // How a node came to be resolvable, so the two very different reasons —
    // "already in the distro, nothing to build" vs. "this project built it"
    // — can be reported as separate counts instead of one combined number
    // that made the distro's own contribution look like this run's work.
    let mut resolution: Vec<Resolution> = nodes
        .iter()
        .map(|n| {
            let rpm = osc::rpm_name(&n.name);
            let key = osc::fuzzy_key(&rpm);
            let facts = node_facts(n, force, finished_roots, is_settled(&rpm), &linked_fuzzy);
            node_resolution(
                &facts,
                || {
                    own_fuzzy
                        .get(&key)
                        .and_then(|spellings| preferred_own_spelling(&rpm, spellings))
                        .map(|real| {
                            // A package OBS built that its dependents cannot install (see
                            // `spec_with_published_defects_fixed`) is not resolved: it is
                            // selected again, repaired and rebuilt.
                            let spec = config
                                .work_dir
                                .join(&config.project)
                                .join(real)
                                .join(format!("{real}.spec"));
                            status_is_resolvable(build_statuses.get(real))
                                && !spec_has_published_defects(&spec, &config.rpms_dir)
                                && !wheel_policy_pending(&config.source_urls, &n.name, &spec)
                        })
                },
                || built_in_state(config, n, &rpm),
            )
        })
        .collect();

    // A distro package counts as resolved by presence alone, which a package
    // built here can contradict by declaring a bound the shipped version
    // misses. Where the distro's version is too old, the version pip resolved
    // is built here instead; where it is too new, the dependent is told to
    // relax the bound (see `distro_bounds`).
    {
        let distro: Vec<bool> = nodes
            .iter()
            .zip(&resolution)
            .map(|(n, r)| {
                *r == Resolution::Distro
                    && linked_fuzzy.contains(&osc::fuzzy_key(&osc::rpm_name(&n.name)))
            })
            .collect();
        let needs_build: Vec<bool> = resolution.iter().map(|r| *r == Resolution::No).collect();
        let plan = distro_bound_plan(
            &obs_project,
            &nodes,
            &distro,
            &needs_build,
            &linked_packages,
            reporter,
        );
        for conflict in &plan.conflicts {
            // Every round derives the same plan: say each conflict once.
            let first = distro_bounds::reported().first_conflict(&obs_project, conflict);
            if first {
                reporter.report(&Event::Warning {
                    step: None,
                    message: conflict.message(&obs_project),
                });
            }
            // The distro's release is used and the dependent's bound dropped
            // from its spec (convention 59); an ABI break shows in its build
            // and goes to a patch (convention 62), not to a dependency that is
            // given up on.
            if conflict.kind == distro_bounds::Kind::DistroTooNew
                && let Some(ws) = Workspace::find_existing(
                    &config.state_root,
                    &[&osc::rpm_name(&conflict.dependent), &conflict.dependent],
                )
            {
                update_state(&ws, reporter, |state| {
                    state.relax_dependency(&conflict.dependency, &conflict.distro_version);
                });
            }
        }
        for note in &plan.notes {
            if distro_bounds::reported().first_note(&obs_project, note) {
                reporter.report(&Event::Warning {
                    step: None,
                    message: note.clone(),
                });
            }
        }
        for &i in &plan.rebuild {
            resolution[i] = Resolution::No;
        }
    }

    // The caller names a given-up package by the OBS spelling it has, which
    // need not match this module's guess — compare by `fuzzy_key`.
    let failed_keys: std::collections::HashSet<String> = permanently_failed
        .iter()
        .map(|p| osc::fuzzy_key(p))
        .collect();
    let resolvable: Vec<bool> = resolution.iter().map(|r| *r != Resolution::No).collect();
    let in_distro_count = resolution
        .iter()
        .filter(|r| **r == Resolution::Distro)
        .count();
    let built_count = resolution
        .iter()
        .filter(|r| **r == Resolution::Built)
        .count();
    let remaining = total - in_distro_count - built_count;

    reporter.report(&Event::ResolveFinished {
        total,
        built: built_count,
        in_distro: in_distro_count,
        remaining,
    });

    // Explicit target override — skip the resolvability gate.
    if let Some(target) = explicit_target {
        let node = find_explicit_target(&nodes, target)?.clone();
        return Ok(vec![TargetInfo {
            name: node.name,
            version: node.version,
            reason: SelectionReason::ExplicitOverride,
            blocked_by: vec![],
        }]);
    }

    // All done.
    if resolvable.iter().all(|&r| r) {
        return Ok(vec![]);
    }

    let root_idx = nodes.iter().position(|n| n.is_requested).unwrap_or(0);

    // Build a normalized name → index map for dep lookups.
    let name_to_idx: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (normalize_pkg_name(&n.name), i))
        .collect();
    let frontier = frontier_of(&nodes, &resolvable, root_idx, &failed_keys);

    if !permanently_failed.is_empty() {
        let edges: Vec<(String, String)> = blocked_dependents(&nodes, &resolvable, &failed_keys)
            .into_iter()
            .map(|(d, b)| (osc::rpm_name(&nodes[d].name), osc::rpm_name(&nodes[b].name)))
            .collect();
        let (_, errors) = sync_blocked_waiters(&config.state_root, &edges);
        for message in errors {
            reporter.report(&Event::Warning {
                step: None,
                message,
            });
        }
    }

    if frontier.is_empty() {
        // Nothing buildable but packages remain. If EVERY still-unresolved
        // package is explained by a `permanently_failed` package — it failed
        // itself, or it depends (transitively, within the closure) on one
        // that did — that's an expected terminal state: the caller reports
        // it and stops rather than treating it as a resolver bug. A
        // `permanently_failed` set that doesn't cover everything left
        // unresolved means something else is also stuck (e.g. an unrelated
        // dependency cycle), which must still surface as an error instead of
        // being silently swallowed just because *some* package gave up.
        let mut explained = vec![false; nodes.len()];
        for (i, node) in nodes.iter().enumerate() {
            if !resolvable[i] && failed_keys.contains(&osc::fuzzy_key(&osc::rpm_name(&node.name))) {
                explained[i] = true;
            }
        }
        loop {
            let mut changed = false;
            for (i, node) in nodes.iter().enumerate() {
                if resolvable[i] || explained[i] {
                    continue;
                }
                let deps = dep_names_from_requires_dist(&node.requires_dist);
                let any_explained_dep = deps
                    .iter()
                    .any(|dep| name_to_idx.get(dep).map(|&j| explained[j]).unwrap_or(false));
                let all_deps_settled = deps.iter().all(|dep| {
                    name_to_idx
                        .get(dep)
                        .map(|&j| resolvable[j] || explained[j])
                        .unwrap_or(true)
                });
                if any_explained_dep && all_deps_settled {
                    explained[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let blocked: Vec<String> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| !resolvable[*i])
            .map(|(_, n)| n.name.clone())
            .collect();

        let all_explained = nodes
            .iter()
            .enumerate()
            .all(|(i, _)| resolvable[i] || explained[i]);
        if all_explained && !permanently_failed.is_empty() {
            return Ok(vec![]);
        }

        return Err(format!(
            "cannot build {name}: stuck — unresolvable packages: {}",
            blocked.join(", ")
        ));
    }

    Ok(frontier)
}
