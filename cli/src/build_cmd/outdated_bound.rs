//! Whether a dependency the queue holds as outdated still needs its update,
//! or the repository's own copy already meets the bound it is asked for.

use super::*;

/// Whether the update path still applies to `dep`, which OBS reports against
/// `outdated`: with no bound it always does; with one, only while the
/// repository's own copy is older than the bound. A copy that meets it is not
/// outdated, so the usual availability check decides what happens to `dep`.
pub(super) fn outdated_still_unmet(
    dep: &str,
    outdated: &OutdatedNames,
    project: &str,
    own_listing: &std::collections::HashSet<String>,
    emit: ap_build::proc::Emit<'_>,
) -> bool {
    let Some(required) = outdated.required(dep) else {
        return true;
    };
    let branch = branch_version(dep, project, own_listing, emit);
    bound_still_unmet(Some(&required), branch.as_deref())
}

/// The version of `dep` in the repository's own spec, when the repository
/// lists the package.
fn branch_version(
    dep: &str,
    project: &str,
    own_listing: &std::collections::HashSet<String>,
    emit: ap_build::proc::Emit<'_>,
) -> Option<String> {
    let real = own_name(dep, own_listing);
    if !own_listing.contains(&real) {
        return None;
    }
    let text = ap_build::update::spec_for_package(project, &real, emit).ok()?;
    ap_build::spec::parse_header(&text).map(|h| h.version)
}

/// The keep/drop decision on a bound: `true` when the repository's copy
/// (`branch`) is older than `required`, so `dep` is updated. Without a bound,
/// or without a known copy, the answer is `true` too: the update path is then
/// what it was before bounds were read.
pub(crate) fn bound_still_unmet(required: Option<&str>, branch: Option<&str>) -> bool {
    match (required, branch) {
        (Some(req), Some(cur)) => ap_build::update::is_newer(req, cur),
        _ => true,
    }
}
