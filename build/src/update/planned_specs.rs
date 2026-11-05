//! The specs a plan read, kept for the update that follows it, so that the
//! package is not read from OBS a second time in the same run.

use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Specs a plan has just read, by (project, package), for [`planned_spec`].
pub(super) static PLANNED_SPECS: Mutex<Option<HashMap<(String, String), String>>> =
    Mutex::new(None);

/// Forget the specs a stopped plan read for packages it did not hand on. Nothing
/// will ask for them, and a later read of such a package must ask OBS rather
/// than take a spec read before the stop.
pub(super) fn discard_stashes(project: &str, read: &BTreeSet<String>, handed: &BTreeSet<String>) {
    let mut guard = PLANNED_SPECS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = guard.as_mut() {
        for pkg in read.difference(handed) {
            m.remove(&(project.to_string(), pkg.clone()));
        }
    }
}

pub(super) fn stash_spec(project: &str, pkg: &str, spec: Option<&String>) {
    if let Some(spec) = spec {
        PLANNED_SPECS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert((project.to_string(), pkg.to_string()), spec.clone());
    }
}

/// [`spec_for_package`] for a project that is only read during this run: the
/// spec a plan fetched a moment ago is handed over, once, instead of fetching
/// it again. Taken rather than kept, so a later read, after the package has
/// been changed, always asks OBS.
pub fn planned_spec(project: &str, pkg: &str, emit: Emit<'_>) -> Result<String, ReadError> {
    let kept = PLANNED_SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&(project.to_string(), pkg.to_string())));
    match kept {
        Some(spec) => Ok(spec),
        None => spec_for_package(project, pkg, emit),
    }
}

/// The answer kept for each package a plan reads, one cell per package.
pub(super) type SpecCells = Mutex<HashMap<String, Arc<Mutex<Option<Result<String, ReadError>>>>>>;

/// `read` gives the answer for `key`, unless one is already kept for it. Callers
/// for one key wait for each other's read, so OBS is asked once. Every answer is
/// kept except a failed read ([`ReadError::Failed`]: OBS did not answer, or the
/// run was interrupted), which the next caller reads again.
pub(super) fn read_once(
    cells: &SpecCells,
    key: &str,
    read: impl FnOnce() -> Result<String, ReadError>,
) -> Result<String, ReadError> {
    let cell = cells
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.to_string())
        .or_default()
        .clone();
    let mut kept = cell.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(answer) = kept.as_ref() {
        return answer.clone();
    }
    let answer = read();
    if !matches!(answer, Err(ReadError::Failed(_))) {
        *kept = Some(answer.clone());
    }
    answer
}
