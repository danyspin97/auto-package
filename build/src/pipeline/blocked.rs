//! Blocked dependents: the packages that wait on a failed one and the sync of
//! their waiting state.

use super::*;

/// Outcome of a package that waits on a given-up (failed) package.
pub const WAITING_ON_FAILED: &str = "waiting_on_failed";

const WAITING_ON_FAILED_PREFIX: &str = "waiting on failed package: ";

/// Unresolved nodes that depend, directly or through other unresolved nodes,
/// on a given-up one, each with the given-up node it waits on (indices into
/// `nodes`). A node that was given up itself is not a dependent.
pub(super) fn blocked_dependents(
    nodes: &[ap_ecosystem::pip::Node],
    resolvable: &[bool],
    failed_keys: &std::collections::HashSet<String>,
) -> Vec<(usize, usize)> {
    let idx: std::collections::HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (normalize_pkg_name(&n.name), i))
        .collect();
    let is_failed = |i: usize| {
        !resolvable[i] && failed_keys.contains(&osc::fuzzy_key(&osc::rpm_name(&nodes[i].name)))
    };
    // blocker[i]: the given-up node i waits on, once known.
    let mut blocker: Vec<Option<usize>> = (0..nodes.len())
        .map(|i| is_failed(i).then_some(i))
        .collect();
    loop {
        let mut changed = false;
        for (i, node) in nodes.iter().enumerate() {
            if resolvable[i] || blocker[i].is_some() {
                continue;
            }
            let found = dep_names_from_requires_dist(&node.requires_dist)
                .iter()
                .filter_map(|d| idx.get(d))
                .find_map(|&j| blocker[j]);
            if found.is_some() {
                blocker[i] = found;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    blocker
        .iter()
        .enumerate()
        .filter_map(|(i, b)| b.filter(|&b| b != i).map(|b| (i, b)))
        .collect()
}

/// Bring every package that waits on a given-up package into
/// `waiting_on_failed`, and free those whose blocker left the blocked state.
/// One function for both directions, used by `select_target` (which knows the
/// dependency graph: `edges` are `(dependent, blocker)` RPM names, each blocker
/// held as blocked for this call even when its own state is `failed`) and by
/// [`reconcile_state`] (state only: `edges` empty).
///
/// A `missing_deps` package whose waited-on dependencies include a blocked or
/// `waiting_on_failed` package waits on that blocker too, so it converts, and
/// so does whatever waits on it in turn (to a fixpoint). A package with its own
/// terminal outcome (`running`, `success`, `failed`, `wheel-only`) is never
/// touched: it keeps its own reason rather than the blocker's. Returns the packages whose outcome
/// changed, and the errors of state writes that failed.
pub(crate) fn sync_blocked_waiters(
    state_root: &Path,
    edges: &[(String, String)],
) -> (Vec<(String, &'static str)>, Vec<String>) {
    use std::collections::{HashMap, HashSet};
    // (outcome, reason)
    type Cur = (Option<String>, Option<String>);
    let mut errors = Vec::new();
    let Ok(rd) = std::fs::read_dir(state_root) else {
        return (Vec::new(), errors);
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut orig: HashMap<String, Cur> = HashMap::new();
    let mut key_of: HashMap<String, String> = HashMap::new();
    for n in &names {
        let Some(ws) = Workspace::find_existing(state_root, &[n.as_str()]) else {
            continue;
        };
        let st = PkgState::load(&ws);
        key_of.insert(osc::fuzzy_key(n), n.clone());
        orig.insert(n.clone(), (st.last_outcome, st.last_failure_reason));
    }
    let mut cur = orig.clone();
    let find = |dep: &str| -> Option<String> {
        [dep.to_string(), osc::rpm_name(dep)]
            .iter()
            .find_map(|d| key_of.get(&osc::fuzzy_key(d)).cloned())
    };
    let held: HashSet<String> = edges.iter().map(|(_, b)| osc::fuzzy_key(b)).collect();
    // The root blocker `name` stands for, when it is blocked or waiting.
    let blocker_of = |cur: &HashMap<String, Cur>, name: &str| -> Option<String> {
        let c = cur.get(name)?;
        match c.0.as_deref() {
            Some(WAITING_ON_FAILED) => {
                c.1.as_deref()
                    .and_then(|r| r.strip_prefix(WAITING_ON_FAILED_PREFIX))
                    .map(|b| b.trim().to_string())
                    .filter(|b| !b.is_empty())
            }
            _ => None,
        }
    };
    let is_blocked = |cur: &HashMap<String, Cur>, b: &str| {
        held.contains(&osc::fuzzy_key(b))
            || find(b).is_some_and(|n| {
                matches!(
                    cur.get(&n).and_then(|c| c.0.as_deref()),
                    Some(WAITING_ON_FAILED)
                )
            })
    };
    // Free: a waiter whose blocker is no longer blocked. Repeat, since a
    // waiter's own blocker may be another waiter that is freed first.
    loop {
        let mut changed = false;
        for n in &names {
            if cur.get(n).and_then(|c| c.0.as_deref()) != Some(WAITING_ON_FAILED) {
                continue;
            }
            let blocker = blocker_of(&cur, n).unwrap_or_default();
            if !blocker.is_empty() && is_blocked(&cur, &blocker) {
                continue;
            }
            cur.insert(
                n.clone(),
                (
                    Some("interrupted".into()),
                    Some(
                        "the package it waited on is no longer given up; ready to run again".into(),
                    ),
                ),
            );
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // Hold: graph dependents of a given-up package.
    for (dependent, blocker) in edges {
        let Some(n) = find(dependent) else { continue };
        // Only a package with no outcome of its own (or a non-terminal one) is
        // held; one that failed or was built as a wheel keeps its own reason.
        if !matches!(
            cur[&n].0.as_deref(),
            None | Some("interrupted" | "missing_deps" | WAITING_ON_FAILED)
        ) {
            continue;
        }
        cur.insert(
            n,
            (
                Some(WAITING_ON_FAILED.into()),
                Some(format!("{WAITING_ON_FAILED_PREFIX}{blocker}")),
            ),
        );
    }
    // Hold: a `missing_deps` package waiting on a blocked one, transitively.
    loop {
        let mut changed = false;
        for n in &names {
            let Some(c) = cur.get(n) else { continue };
            if c.0.as_deref() != Some("missing_deps") {
                continue;
            }
            let Some(list) =
                c.1.as_deref()
                    .and_then(|r| r.strip_prefix("waiting on dependencies: "))
            else {
                continue;
            };
            let root = list
                .split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .find_map(|d| {
                    find(d)
                        .and_then(|dn| {
                            blocker_of(&cur, &dn)
                                .or_else(|| held.contains(&osc::fuzzy_key(&dn)).then(|| dn.clone()))
                        })
                        .or_else(|| held.contains(&osc::fuzzy_key(d)).then(|| d.to_string()))
                });
            if let Some(root) = root {
                cur.insert(
                    n.clone(),
                    (
                        Some(WAITING_ON_FAILED.into()),
                        Some(format!("{WAITING_ON_FAILED_PREFIX}{root}")),
                    ),
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = Vec::new();
    for n in names {
        let (Some(was), Some(now)) = (orig.get(&n), cur.get(&n)) else {
            continue;
        };
        if was == now {
            continue;
        }
        let new: &'static str = if now.0.as_deref() == Some(WAITING_ON_FAILED) {
            WAITING_ON_FAILED
        } else {
            "interrupted"
        };
        let Some(ws) = Workspace::find_existing(state_root, &[n.as_str()]) else {
            continue;
        };
        let (outcome, reason) = now.clone();
        // Set only when the closure actually writes: a run may have started
        // since the states were read, and then nothing changed.
        let mut written = false;
        match PkgState::update(&ws, |st| {
            // A run may have started since the states were read.
            if matches!(st.last_outcome.as_deref(), Some("running" | "success")) {
                return;
            }
            st.last_outcome = outcome;
            st.last_failure_reason = reason;
            written = true;
        }) {
            Ok(_) => {
                if written && was.0 != now.0 {
                    out.push((n, new));
                }
            }
            Err(e) => errors.push(format!("saving state.json of {n}: {e}")),
        }
    }
    (out, errors)
}
