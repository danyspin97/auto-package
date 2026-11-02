//! Whether a package's origin can build at all. A branch of an origin that
//! is unresolvable in every repository cannot build either: the update has
//! nothing to work from, so the run skips it before branching.

use crate::obs_status::ResultEntry;
use crate::osc::failure::extract_unresolvable;
use crate::osc::{BuildStatus, get_package_results_if_present};

/// The reason to skip `pkg` when its results in the origin say it is
/// unresolvable in every repository, or `None` when it should be branched.
/// Any other state is left to the update: a failed build may be fixed by a
/// new revision, a succeeded repository builds, and a dirty result is an
/// older revision's verdict. No results at all is a new package.
pub fn origin_unresolvable_reason(entries: &[ResultEntry], pkg: &str) -> Option<String> {
    let own: Vec<&ResultEntry> = entries.iter().filter(|e| e.pkg == pkg).collect();
    let all_unresolvable = !own.is_empty()
        && own
            .iter()
            .all(|e| !e.dirty && BuildStatus::from(e.code.as_str()) == BuildStatus::Unresolvable);
    if !all_unresolvable {
        return None;
    }
    let details: Vec<String> = own.iter().filter_map(|e| e.details.clone()).collect();
    let names = extract_unresolvable(&details.join("\n"));
    Some(if names.is_empty() {
        "OBS reports it unresolvable in every repository".to_string()
    } else {
        format!("nothing provides {}", names.join(", "))
    })
}

/// [`origin_unresolvable_reason`] for the origin's results as OBS has them now.
/// A read that fails is an error, which the caller treats as "branch anyway".
pub fn origin_unresolvable(origin: &str, pkg: &str) -> Result<Option<String>, String> {
    let entries = get_package_results_if_present(origin, pkg)?;
    Ok(entries.and_then(|e| origin_unresolvable_reason(&e, pkg)))
}
