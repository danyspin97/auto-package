//! The leftovers of a branch package's checkout, cleaned in one place by the
//! commit step and by the tidy commit: the unpublished `.changes` entries
//! normalised against the origin (dependency and placeholder bullets among
//! them), and the directories a service run left at the top of the checkout.
//! Stray files, old archives and the quilt `series` are not here: the commit
//! removes them after its publish plan, and refuses some plans before it does.

use super::changes_normalise::normalise_checkout;
use super::service_dirs::remove_service_source_dirs;
use super::*;

/// Cleans the leftovers of the branch package `pkg` of `project`, checked out
/// in `checkout`. `named` is what the expanded spec names. Returns one message
/// per change made, empty when nothing was left to clean. Best effort: an
/// origin that cannot be read leaves its `.changes` as it is.
pub fn clean_branch_leftovers(
    checkout: &Path,
    project: &str,
    pkg: &str,
    named: &[String],
    emit: Emit<'_>,
) -> Vec<String> {
    let origin_changes = match link_origin(project, pkg, emit) {
        Ok(Some(origin)) => cat_file(
            &origin.project,
            &origin.package,
            &format!("{}.changes", origin.package),
            emit,
        ),
        _ => None,
    };
    clean_with_origin(checkout, origin_changes.as_deref(), pkg, named)
}

/// [`clean_branch_leftovers`] with the origin's `.changes` text given (`None`
/// when the package has no readable origin: its `.changes` is left alone).
pub(super) fn clean_with_origin(
    checkout: &Path,
    origin_changes: Option<&str>,
    pkg: &str,
    named: &[String],
) -> Vec<String> {
    let mut messages = origin_changes
        .map(|text| normalise_checkout(checkout, text, pkg))
        .unwrap_or_default();
    for name in remove_service_source_dirs(checkout, named) {
        messages.push(format!(
            "deleted '{name}/' from the checkout: a service left it there"
        ));
    }
    messages
}
