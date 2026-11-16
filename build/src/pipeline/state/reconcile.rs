//! The startup reconcile of the state with OBS and with the checkout: what
//! `state.json` says unfinished that the state directory can settle, and the
//! RPMs OBS built for a package that its state records as unfinished.

use super::*;

/// Bring every package's recorded outcome in line with reality at startup.
///
/// - `running` left by a process that died (nothing holds the package's
///   `build.lock`) becomes `interrupted`, so the package reads as re-runnable.
/// - `missing_deps` whose waited-on dependencies are all built (their own
///   state says `success`) becomes `interrupted` too: the wait is over.
///
/// Returns the packages changed, as `(name, new outcome)`.
pub fn reconcile_state(
    state_root: &Path,
    covers: impl Fn(&str) -> bool,
) -> Vec<(String, &'static str)> {
    let mut changed = Vec::new();
    let Ok(rd) = std::fs::read_dir(state_root) else {
        return changed;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("state.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| covers(n))
        .collect();
    names.sort();
    // A dependency outside `covers` is not read: it is not taken as built.
    let built = |dep: &str| {
        [dep.to_string(), osc::rpm_name(dep)].iter().any(|n| {
            covers(n)
                && Workspace::find_existing(state_root, &[n.as_str()])
                    .is_some_and(|w| PkgState::load(&w).last_outcome.as_deref() == Some("success"))
        })
    };
    for name in names {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let state = PkgState::load(&ws);
        let (new, reason) = match state.last_outcome.as_deref() {
            Some("running") => {
                // Only a lock we can take proves the owner is gone.
                if ap_core::proc_lock::try_lock(&ws.dir().join("build.lock")).is_err() {
                    continue;
                }
                (
                    "interrupted",
                    "the run that was building this package ended without a result",
                )
            }
            Some("missing_deps") => {
                let waiting: Vec<String> = state
                    .last_failure_reason
                    .as_deref()
                    .and_then(|r| r.strip_prefix("waiting on dependencies: "))
                    .map(|d| d.split(',').map(|x| x.trim().to_string()).collect())
                    .unwrap_or_default();
                if waiting.is_empty() || !waiting.iter().all(|d| built(d)) {
                    continue;
                }
                (
                    "interrupted",
                    "the dependencies it waited on are built; ready to run again",
                )
            }
            _ => continue,
        };
        if PkgState::update(&ws, |st| {
            st.last_outcome = Some(new.to_string());
            st.last_failure_reason = Some(reason.to_string());
        })
        .is_ok()
        {
            changed.push((name, new));
        }
    }
    // Dependents that wait on a blocked package, and those whose blocker is
    // no longer blocked.
    let (more, _errors) = sync_blocked_waiters(state_root, &[], &covers);
    for (name, outcome) in more {
        changed.retain(|(n, _)| *n != name);
        changed.push((name, outcome));
    }
    changed.retain(|(n, _)| covers(n));
    changed
}

/// A package whose `state.json` records an unfinished outcome although OBS shows
/// its build succeeded is fixed here, before the run builds on it: the RPMs are
/// downloaded (`download(name)`, the number fetched) and the outcome becomes
/// `success`. The decision is [`crate::heal::obs_built_decision`]: only the
/// version the state last built, with a clean checkout, is taken. `statuses`
/// is keyed by lower-case package name. `download` gets the package's OBS name.
/// Only the packages `covers` names are fixed. Returns `(name, Ok(rpmlint
/// warnings))` for each package fixed and `(name, Err(problem))` for each that
/// OBS built but could not be fixed; a package that is declined is not listed
/// (its `self_check` finding says why).
pub fn reconcile_with_obs(
    state_root: &Path,
    checkout_root: &Path,
    rpms_dir: &Path,
    statuses: &std::collections::HashMap<String, osc::BuildStatus>,
    local_changes: impl Fn(&Path) -> bool,
    download: impl Fn(&str) -> Result<usize, osc::Error>,
    covers: impl Fn(&str) -> bool,
) -> Vec<(String, Result<Vec<String>, String>)> {
    let mut out = Vec::new();
    for name in crate::heal::package_names(state_root)
        .into_iter()
        .filter(|n| covers(n))
    {
        let Some(ws) = Workspace::find_existing(state_root, &[&name]) else {
            continue;
        };
        let state = PkgState::load(&ws);
        // The checkout and the OBS download are named by the OBS name the state
        // records; a state written before that field is the name itself.
        let obs = state.obs_name.clone().unwrap_or_else(|| name.clone());
        let checkout = checkout_root.join(&obs);
        let spec_version = std::fs::read_to_string(checkout.join(format!("{obs}.spec")))
            .ok()
            .and_then(|t| crate::spec::parse_header(&t).map(|h| h.version));
        let decision = crate::heal::obs_built_decision(
            state.last_outcome.as_deref(),
            statuses.get(&name.to_lowercase()),
            spec_version.as_deref(),
            state.last_built_version.as_deref(),
            || local_changes(&checkout),
        );
        let (crate::heal::ObsDecision::Fetch, Some(version)) = (decision, spec_version) else {
            continue;
        };
        match download(&obs) {
            Ok(n) if n > 0 => {
                // The rpmlint log of what was fetched, as a normal download does.
                let record = record_rpmlint_logs(rpms_dir, &obs, &version, &ws);
                let lint: Vec<String> =
                    record.findings.into_iter().chain(record.warnings).collect();
                let what = format!("recorded success: OBS built {version}, fetched {n} RPM(s)");
                let saved = PkgState::update(&ws, |st| {
                    st.last_built_version = Some(version.clone());
                    st.last_outcome = Some("success".to_string());
                    st.last_failure_reason = None;
                });
                match saved {
                    Ok(_) => {
                        crate::heal::record(&ws, &what);
                        out.push((name, Ok(lint)));
                    }
                    Err(e) => out.push((
                        name,
                        Err(format!(
                            "OBS built it; fetched {n} RPM(s), state not saved: {e}"
                        )),
                    )),
                }
            }
            Ok(_) => out.push((
                name,
                Err("OBS built it but no RPMs were downloaded".to_string()),
            )),
            Err(e) => out.push((
                name,
                Err(format!("OBS built it but fetching its RPMs failed: {e}")),
            )),
        }
    }
    out
}

#[cfg(test)]
#[path = "reconcile_scope_tests.rs"]
mod scope_tests;
