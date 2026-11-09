//! Which listed package a dependency of an updated package names.

use ap_build::osc::{Listed, ambiguity_message, find_listed};

/// The listed spelling `pkg` names among `names`, as [`find_listed`] picks it.
/// `Ok(None)` when no listed name is the same project, `Err` when several
/// listed names are and none is `pkg` up to case: the caller reports that
/// instead of choosing one, as [`ap_build::osc::obs_name`] does.
pub(super) fn listed_spelling<'a>(pkg: &str, names: &[&'a str]) -> Result<Option<&'a str>, String> {
    match find_listed(pkg, names.iter().copied()) {
        Listed::Found(name) => Ok(Some(name)),
        Listed::Absent => Ok(None),
        Listed::Ambiguous(same) => Err(ambiguity_message(pkg, &same)),
    }
}
