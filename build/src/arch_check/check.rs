//! The check itself: add the missing architectures to the branch (only when
//! asked to), wait for the other-architecture builds, and classify what they
//! say. The caller reports the rows. Every OBS read and write goes through
//! [`ArchObs`].

use std::cell::RefCell;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use super::*;
use crate::obs_status::{ResultEntry, resolve_lenient};
use crate::osc::{BuildStatus, parse_project_info};

/// What one check is asked to do for a package.
pub struct CheckRequest<'a> {
    /// The branch project the package was built in (the only one written to).
    pub branch: &'a str,
    /// The project the branch was made from.
    pub origin: &'a str,
    pub pkg: &'a str,
    /// The `(repository, arch)` the package was built for; not checked again.
    pub primary: (&'a str, &'a str),
    /// `--arches`, when given: the set to check, narrowed.
    pub wanted: Option<&'a [String]>,
    /// Add the origin's architectures the branch lacks, before the check. OBS
    /// then schedules every package of the branch on them, so this is opt-in.
    pub add_arches: bool,
    /// The ports to add to the branch (see `ports`), and enable for the
    /// package once they are there. Empty for none.
    pub ports: Vec<Port>,
    /// The run checks the Factory ports (`--ports`). Without it, a port
    /// repository the branch already has (from an earlier run) is neither
    /// checked nor waited for; with it, every repository of the branch is.
    pub include_ports: bool,
    /// The branch is only read here (the source of an in-place update): no
    /// architecture or port is added to it, whatever the other fields say.
    pub read_only: bool,
    /// The deadline the other-architecture waits of the run share (see
    /// [`ArchDeadline`]): the checks of its packages wait together.
    pub deadline: &'a ArchDeadline,
    /// Held while the branch project's meta is written, so the checks of the
    /// run's packages do not rewrite it at once. Not held for the waits.
    pub meta_lock: &'a Mutex<()>,
    /// Where the logs of failures are saved.
    pub log_dir: &'a Path,
    /// Told each thing before it is done: an addition before the write.
    pub say: &'a dyn Fn(String),
}

/// What a check did: the rows it found, the architectures it added to the
/// branch and notes for the user.
#[derive(Debug, Default)]
pub struct CheckOutcome {
    pub rows: Vec<ArchRow>,
    pub added: Vec<MetaEdit>,
    pub notes: Vec<String>,
    /// Every architecture of the branch, but the primary, was checked and
    /// waited for. Only then is the package's build status the whole story.
    pub covered: bool,
}

type Key = (String, String);

/// Run the check described by `req`. An error means the check could not run
/// at all (a project could not be read, the branch could not be written); the
/// package's own build is not affected by it.
pub fn check_other_arches<O: ArchObs + ?Sized>(
    obs: &O,
    req: &CheckRequest<'_>,
) -> Result<CheckOutcome, String> {
    let writes = req.add_arches || !req.ports.is_empty();
    if writes && !req.read_only {
        // Refused before any OBS read: only a home: project is ever written.
        ensure_branch_project(req.branch)?;
    }
    let origin_xml = obs
        .project_meta(req.origin)?
        .ok_or_else(|| format!("the origin project {} does not exist", req.origin))?;
    let origin = parse_project_info(&origin_xml);
    let mut notes: Vec<String> = Vec::new();
    let mut added: Vec<MetaEdit> = Vec::new();
    if writes && req.read_only {
        notes.push(
            "the branch is only read here: no architecture or port is added to it".to_string(),
        );
    } else if writes {
        let _write = req.meta_lock.lock().unwrap_or_else(|p| p.into_inner());
        added = edit_branch_meta(
            obs,
            req.branch,
            |branch| {
                let mut edits: Vec<MetaEdit> = if req.add_arches {
                    plan_additions(branch, &origin, req.wanted)
                        .into_iter()
                        .map(MetaEdit::Arch)
                        .collect()
                } else {
                    Vec::new()
                };
                edits.extend(req.ports.iter().map(|p| MetaEdit::Port(*p)));
                edits
            },
            |edits| {
                for e in edits {
                    (req.say)(e.describe(req.branch));
                }
            },
        )?;
        notes.extend(added.iter().map(|e| match e {
            MetaEdit::Arch(a) => format!("added {} to {}/{}", a.arch, req.branch, a.repository),
            MetaEdit::Port(p) => format!("added {} to {}", p.repo, req.branch),
        }));
    }
    let arch_added: Vec<ArchAddition> = added
        .iter()
        .filter_map(|e| match e {
            MetaEdit::Arch(a) => Some(a.clone()),
            MetaEdit::Port(_) => None,
        })
        .collect();
    let branch_xml = obs
        .project_meta(req.branch)?
        .ok_or_else(|| format!("the branch project {} does not exist", req.branch))?;
    let branch = parse_project_info(&branch_xml);
    if !req.ports.is_empty() && !req.read_only {
        // After the ports are in the branch, and before the results are read.
        let repos: Vec<&str> = req.ports.iter().map(|p| p.repo).collect();
        if let Err(e) = obs.enable_repositories(req.branch, req.pkg, &repos) {
            notes.push(format!(
                "could not enable {} for {}: its port results are not checked: {e}",
                repos.join(", "),
                req.pkg
            ));
        }
    }

    let keys: Vec<Key> = check_set(&branch, &origin, &arch_added, req.wanted)
        .into_iter()
        .filter(|(r, a)| (r.as_str(), a.as_str()) != req.primary)
        .filter(|(r, _)| req.include_ports || !is_port_repo(r))
        .collect();
    // A port is not needed to cover the branch, checked or not.
    let covered = branch
        .repositories
        .iter()
        .filter(|repo| !is_port_repo(&repo.name))
        .all(|repo| {
            repo.arches.iter().all(|arch| {
                (repo.name.as_str(), arch.as_str()) == req.primary
                    || keys.iter().any(|(r, a)| *r == repo.name && a == arch)
            })
        });
    if keys.is_empty() {
        return Ok(CheckOutcome {
            rows: Vec::new(),
            added,
            notes,
            covered,
        });
    }

    let (rows, waited) = wait_and_classify(obs, req, &keys)?;
    notes.extend(waited);
    let rows = fetch_failure_details(obs, req, rows);
    let rows = judge_baselines(obs, req, rows, &mut notes);
    Ok(CheckOutcome {
        rows,
        added,
        notes,
        covered,
    })
}

/// Poll the branch's results for `keys` until none is pending or the bound
/// passes, then classify each one. Returns the rows and the notes of the wait.
fn wait_and_classify<O: ArchObs + ?Sized>(
    obs: &O,
    req: &CheckRequest<'_>,
    keys: &[Key],
) -> Result<(Vec<ArchRow>, Vec<String>), String> {
    let latest: RefCell<Vec<ResultEntry>> = RefCell::new(Vec::new());
    let statuses = wait_settled(
        || req.deadline.left(Instant::now()),
        || {
            let entries = obs.results(req.branch, req.pkg)?;
            let out = keys
                .iter()
                .map(|k| (k.clone(), status_of(&entries, req.pkg, k)))
                .collect();
            *latest.borrow_mut() = entries;
            Ok(out)
        },
        crate::osc::sleep_interruptible,
        crate::osc::poll_pause,
    )?;
    let entries = latest.into_inner();
    let mut notes = Vec::new();
    if statuses.iter().any(|(_, s)| !s.is_terminal()) {
        notes.push(format!(
            "still building when the wait ended: those results are not final"
        ));
    }
    let rows = keys
        .iter()
        .map(|key| {
            let (pkg, status, details) = match deciding_entry(&entries, req.pkg, key) {
                Some(e) => (e.pkg.clone(), resolve_lenient(e), e.details.clone()),
                None => (
                    req.pkg.to_string(),
                    BuildStatus::Unknown("no result yet".to_string()),
                    None,
                ),
            };
            ArchRow {
                pkg,
                repo: key.0.clone(),
                arch: key.1.clone(),
                state: state_in(&key.0, &status, details.as_deref()),
                // Judged by `judge_baselines` for a failure. A failure whose
                // baseline cannot be read keeps this and blocks, as before.
                baseline: BaselineState::Builds,
                log: None,
            }
        })
        .collect();
    Ok((rows, notes))
}

/// The entry of `entries` that decides the result of `pkg` at `key`: the
/// package or one of its `<package>:<flavor>` multibuild entries, in the
/// repository and arch. A failing one decides, else one still going, else a
/// succeeded one. `None` when no entry reports `key`.
pub(super) fn deciding_entry<'a>(
    entries: &'a [ResultEntry],
    pkg: &str,
    key: &Key,
) -> Option<&'a ResultEntry> {
    let flavor_prefix = format!("{pkg}:");
    let mut best: Option<(u8, &ResultEntry)> = None;
    for e in entries.iter().filter(|e| {
        (e.pkg == pkg || e.pkg.starts_with(&flavor_prefix)) && e.repo == key.0 && e.arch == key.1
    }) {
        let state = classify(&resolve_lenient(e), e.details.as_deref());
        let rank = match state {
            ArchState::Failed { .. } | ArchState::Unresolvable { .. } | ArchState::Broken => 4,
            ArchState::Pending(_) => 3,
            ArchState::Succeeded => 2,
            _ => 1,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, e));
        }
    }
    best.map(|(_, e)| e)
}

/// The status of `(repo, arch)` for `pkg` in `entries`; a result not reported
/// yet is pending, not absent.
pub(super) fn status_of(entries: &[ResultEntry], pkg: &str, key: &Key) -> BuildStatus {
    deciding_entry(entries, pkg, key)
        .map(resolve_lenient)
        .unwrap_or_else(|| BuildStatus::Unknown("no result yet".to_string()))
}

/// Fetch the build log of each failed row and take its first error line as the
/// row's detail. A log that cannot be fetched leaves the row as it is.
fn fetch_failure_details<O: ArchObs + ?Sized>(
    obs: &O,
    req: &CheckRequest<'_>,
    rows: Vec<ArchRow>,
) -> Vec<ArchRow> {
    rows.into_iter()
        .map(|mut row| {
            if !row.state.is_failure() {
                return row;
            }
            let path = req.log_dir.join(format!(
                "{}-{}-{}.log",
                ap_core::run::sanitize_name(&row.pkg),
                ap_core::run::sanitize_name(&row.repo),
                ap_core::run::sanitize_name(&row.arch),
            ));
            if obs
                .buildlog(req.branch, &row.pkg, &row.repo, &row.arch, &path)
                .is_err()
            {
                return row;
            }
            row.log = Some(path.display().to_string());
            if let Ok(text) = std::fs::read_to_string(&path)
                && let ArchState::Failed { .. } = row.state
            {
                row.state = ArchState::Failed {
                    detail: first_error_line(&text),
                };
            }
            row
        })
        .collect()
}

/// The project a failure is compared with: the origin, or for a port repository
/// the port project.
fn baseline_project(origin: &str, repo: &str) -> String {
    port_for_repo(repo).map_or_else(|| origin.to_string(), |p| p.project.to_string())
}

/// The state a baseline reports for `key`: `None` when it has no result row of
/// the package there. An excluded or disabled result is kept, so it reads as
/// [`BaselineState::Excluded`].
pub(super) fn baseline_state(entries: &[ResultEntry], pkg: &str, key: &Key) -> Option<ArchState> {
    deciding_entry(entries, pkg, key).map(|e| classify(&resolve_lenient(e), e.details.as_deref()))
}

/// Read each failure's baseline, once per project: the origin for an ordinary
/// failure, the port project for a port's, judged by [`judge_port`]. A baseline
/// OBS has no package for is no result; one that cannot be read marks nothing,
/// and says so in the notes.
fn judge_baselines<O: ArchObs + ?Sized>(
    obs: &O,
    req: &CheckRequest<'_>,
    mut rows: Vec<ArchRow>,
    notes: &mut Vec<String>,
) -> Vec<ArchRow> {
    let mut projects: Vec<String> = rows
        .iter()
        .filter(|r| r.state.is_failure())
        .map(|r| baseline_project(req.origin, &r.repo))
        .collect();
    projects.sort();
    projects.dedup();
    let reads: Vec<(String, Result<Option<Vec<ResultEntry>>, String>)> = projects
        .into_iter()
        .map(|p| {
            let read = obs.results_if_present(&p, req.pkg);
            (p, read)
        })
        .collect();
    for row in rows.iter_mut().filter(|r| r.state.is_failure()) {
        let project = baseline_project(req.origin, &row.repo);
        let entries: &[ResultEntry] = match reads.iter().find(|(p, _)| *p == project) {
            Some((_, Ok(Some(entries)))) => entries,
            // A project or package OBS does not have is no result to compare with.
            Some((_, Ok(None))) => &[],
            Some((_, Err(e))) => {
                notes.push(format!("the results of {project} could not be read: {e}"));
                continue;
            }
            None => continue,
        };
        let key = baseline_key(&row.repo, &row.arch);
        let base = baseline_state(entries, req.pkg, &key);
        if is_port_repo(&row.repo) {
            let (state, baseline) = judge_port(row.state.clone(), base.as_ref());
            row.state = state;
            row.baseline = baseline;
        } else {
            row.baseline = BaselineState::of(base.as_ref());
        }
    }
    rows
}

/// Every result of `pkg` in `project` with its state, one per `(repo, arch)`,
/// read the way [`wait_and_classify`] reads them: the package's own entries
/// and its flavors' decide together, so a failing flavor is a failure.
pub fn read_results<O: ArchObs + ?Sized>(
    obs: &O,
    project: &str,
    pkg: &str,
) -> Result<Vec<(Key, ArchState)>, String> {
    let entries = obs.results(project, pkg)?;
    let mut keys: Vec<Key> = entries
        .iter()
        .filter(|e| e.pkg == pkg || e.pkg.starts_with(&format!("{pkg}:")))
        .map(|e| (e.repo.clone(), e.arch.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys
        .into_iter()
        .map(|key| {
            let state = match deciding_entry(&entries, pkg, &key) {
                Some(e) => state_in(&key.0, &resolve_lenient(e), e.details.as_deref()),
                None => ArchState::Pending("no result yet".to_string()),
            };
            (key, state)
        })
        .collect())
}

/// The rows the submit gate decides on for `pkg`: each result of the branch.
/// A failure carries its baseline: the origin's result for the same `(repo,
/// arch)`, or for a port the port project's, which also judges the port's
/// failure (see [`judge_port`]). A baseline is read only for a failure.
///
/// A port repository is a row only when `include_ports` is set, the run having
/// checked the ports (see `check_other_arches`): a port the run did not check
/// is not read, so an earlier run's result of it cannot refuse a submit.
pub fn gate_inputs<O: ArchObs + ?Sized>(
    obs: &O,
    branch: &str,
    origin: &str,
    pkg: &str,
    include_ports: bool,
) -> Result<Vec<GateRow>, String> {
    let mut rows: Vec<GateRow> = read_results(obs, branch, pkg)?
        .into_iter()
        .filter(|((repo, _), _)| include_ports || !is_port_repo(repo))
        .map(|((repo, arch), state)| GateRow {
            repo,
            arch,
            state,
            baseline: None,
        })
        .collect();
    let mut projects: Vec<String> = rows
        .iter()
        .filter(|r| r.state.is_failure())
        .map(|r| baseline_project(origin, &r.repo))
        .collect();
    projects.sort();
    projects.dedup();
    let mut reads = Vec::new();
    for project in projects {
        // A baseline OBS has no package for is no result, not a failed gate.
        let entries = obs.results_if_present(&project, pkg)?.unwrap_or_default();
        reads.push((project, entries));
    }
    for row in rows.iter_mut().filter(|r| r.state.is_failure()) {
        let project = baseline_project(origin, &row.repo);
        let entries: &[ResultEntry] = reads
            .iter()
            .find(|(p, _)| *p == project)
            .map_or(&[], |(_, e)| e.as_slice());
        let key = baseline_key(&row.repo, &row.arch);
        let base = baseline_state(entries, pkg, &key);
        if is_port_repo(&row.repo) {
            row.state = judge_port(row.state.clone(), base.as_ref()).0;
        }
        row.baseline = Some(Baseline {
            project,
            state: base,
        });
    }
    Ok(rows)
}
