//! The packages this process finished `Built`. A wait on one of them whose
//! commit is still missing from the repository's copy says so in its message
//! (see `wait_record::precheck_with`), so the cause is visible.

use super::rpm_names::same_package;
use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

static BUILT: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(Default::default);

/// Records that the package `rpm_name` finished `Built` in this run.
pub fn note_built(rpm_name: &str) {
    if let Ok(mut built) = BUILT.lock() {
        built.insert(rpm_name.to_string());
    }
}

/// Whether the package `dep` names finished `Built` in this run.
pub fn built_in_this_run(dep: &str) -> bool {
    BUILT
        .lock()
        .is_ok_and(|built| built.iter().any(|n| same_package(n, dep)))
}
