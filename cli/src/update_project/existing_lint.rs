//! A `lint` of a package the branch project already holds. When the branch's
//! spec is newer than the origin's (an earlier `update`), the lint continues
//! from the branch: it keeps that version and state and improves it, and does
//! not rebase it onto the origin, which would discard the update. A branch at
//! the origin's version or behind it is rebased as before. A branch whose
//! version cannot be compared with the origin's, or whose copy or the origin's
//! cannot be read, is left alone: a rebase could discard its update.

use super::existing::scheme_of;
use super::*;
use ap_build::update_project::{branch_ahead_of_origin, branch_incomparable, branch_version};
use ap_build::version::Scheme;

/// What the branch project's copy means for a lint of an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Continuation {
    /// The branch is branched from, or rebased onto, the origin as before.
    Origin,
    /// The branch holds a newer version than the origin: it is continued from,
    /// at that version.
    Branch { version: String },
    /// The package is left alone, for the reason given.
    Skip(String),
}

/// Whether the decision is made for a run at all: a `lint` (`--improve`) in a
/// branch project, not `--in-place` and not pinned to a revision.
pub(super) fn applies(a: &UpdateProjectArgs) -> bool {
    a.improve && !a.in_place && a.backport_job.is_none() && a.revision.is_none()
}

/// Decide from the versions of the specs. `origin` reads the origin's
/// `Version:`, and `held` the branch's (`Ok(None)` when it holds no package).
/// The origin is read only for a branch that holds the package; when that read
/// fails the package is skipped, not rebased, since a rebase could discard the
/// branch's update.
pub(super) fn decide(
    item: &Item,
    a: &UpdateProjectArgs,
    origin: &dyn Fn() -> Result<String, String>,
    held: &dyn Fn() -> Result<Option<String>, String>,
) -> Continuation {
    if !applies(a) {
        return Continuation::Origin;
    }
    decide_versions(scheme_of(item), origin, held)
}

/// [`decide`] for the versions alone, compared in `scheme`.
pub(super) fn decide_versions(
    scheme: Scheme,
    origin: &dyn Fn() -> Result<String, String>,
    held: &dyn Fn() -> Result<Option<String>, String>,
) -> Continuation {
    let held = match held() {
        Ok(Some(held)) => held,
        Ok(None) => return Continuation::Origin,
        Err(why) => return Continuation::Skip(format!("its copy could not be read: {why}")),
    };
    let origin = match origin() {
        Ok(origin) => origin,
        Err(why) => {
            return Continuation::Skip(format!("the origin's copy could not be read: {why}"));
        }
    };
    if branch_ahead_of_origin(&origin, &held, scheme) {
        Continuation::Branch { version: held }
    } else if branch_incomparable(&origin, &held, scheme) {
        Continuation::Skip(format!(
            "its version {held} cannot be compared with the origin's {origin}"
        ))
    } else {
        Continuation::Origin
    }
}

/// The `Version:` of the package `pkg` in `project`, as its spec holds it.
pub(super) fn origin_version(project: &str, pkg: &str, emit: Emit<'_>) -> Result<String, String> {
    update::spec_for_package(project, pkg, emit)
        .map_err(|e| e.to_string())
        .and_then(|t| {
            ap_build::spec::parse_header(&t)
                .map(|h| h.version)
                .ok_or_else(|| "its spec has no readable Version:".to_string())
        })
}

/// Set `item` to `version`, the version its branch holds. An index address
/// names the release it packages, so the origin's version in it is replaced
/// too; otherwise the build would fetch the origin's archive. Git and PyPI
/// sources name no version, so they are left as they are.
pub(super) fn continue_at(item: &mut Item, version: String) {
    if let Ecosystem::Index { url } = &mut item.ecosystem {
        *url = url.replace(&item.old_version, &version);
    }
    item.old_version = version.clone();
    item.new_version = version;
}

/// [`decide`] against OBS. An item continued from the branch is set to the
/// branch's version, so the run improves that version and does not fetch the
/// origin's. `None` when the package is left alone (listed as skipped).
pub(super) fn continue_from_branch(
    item: &mut Item,
    branch: &str,
    a: &UpdateProjectArgs,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> Option<bool> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let decision = decide(
        item,
        a,
        &|| origin_version(&item.origin, &item.pkg, emit),
        &|| branch_version(branch, &item.pkg, emit),
    );
    match decision {
        Continuation::Origin => Some(false),
        Continuation::Branch { version } => {
            ctx.say(format!(
                "{branch} holds {} {version}, newer than {} in {}; continuing from it, \
                 not rebasing it onto its origin",
                item.pkg, item.old_version, item.origin
            ));
            continue_at(item, version);
            Some(true)
        }
        Continuation::Skip(why) => {
            let why = format!("{branch} is left alone for it: {why}");
            ctx.warn(format!("{}: {why}", item.pkg));
            report.results.push(ItemResult {
                item: item.clone(),
                outcome: ItemOutcome::Skipped(why),
                sr: SrOutcome::NotRequested,
            });
            None
        }
    }
}

#[cfg(test)]
#[path = "existing_lint_tests.rs"]
mod tests;
