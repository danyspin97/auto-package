//! The branch project's own copy of a package, read before the package is
//! branched. A branch at upstream's version whose build succeeded is listed as
//! updated, so its submit request can still be made, but is not rebuilt. A
//! branch at upstream's version whose build did not succeed is continued from,
//! and so is rebuilt. A branch ahead of upstream is listed as needing the
//! maintainer, so an older version does not replace it. The check is made for
//! an upgrade of a package of a plain `update` only.

use super::*;
use ap_build::update_project::{BranchState, branch_state, branch_version};
use ap_build::version::Scheme;
use ap_core::event::PackageOutcome;

/// What the branch project's copy means for an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Settlement {
    /// The item goes on: it is branched, or continued from the branch. `held`
    /// is the version the branch holds, when the item continues from it: the
    /// build starts from that version. `None` when the branch does not hold the
    /// package, or its copy was not read.
    Go { held: Option<String> },
    /// The branch holds upstream's version and it built: not rebuilt.
    Updated,
    /// The branch holds a version newer than upstream's.
    Ahead { branch: String },
    /// The branch's copy or its build could not be read: the package is left
    /// alone, with the cause.
    Unreadable(String),
}

/// Whether the check is made for an item at all: the upgrade of a package in a
/// plain `update`. A `lint --improve` item and a backport item are built at the
/// version the package has (old and new version are the same), `--in-place`
/// has no branch, and `--revision` pins the sources, not the version.
pub(super) fn applies(item: &Item, a: &UpdateProjectArgs) -> bool {
    !a.in_place
        && !a.improve
        && a.backport_job.is_none()
        && a.revision.is_none()
        && item.old_version != item.new_version
}

/// The version scheme the item's versions are compared in.
pub(super) fn scheme_of(item: &Item) -> Scheme {
    scheme_of_ecosystem(&item.ecosystem)
}

/// The version scheme of a package of `ecosystem`.
pub(super) fn scheme_of_ecosystem(ecosystem: &Ecosystem) -> Scheme {
    match ecosystem {
        Ecosystem::Pypi { .. } => Scheme::Pep440,
        _ => Scheme::Generic,
    }
}

/// Decide from the branch's copy. `held` reads the `Version:` the branch holds
/// (`Ok(None)` when it holds no package) and `built` the build status of the
/// branch's package; `built` is read only for a branch that holds upstream's
/// version, once.
pub(super) fn settle(
    item: &Item,
    a: &UpdateProjectArgs,
    held: &dyn Fn() -> Result<Option<String>, String>,
    built: &dyn Fn() -> Result<osc::BuildStatus, String>,
) -> Settlement {
    if !applies(item, a) {
        return Settlement::Go { held: None };
    }
    let held = match held() {
        Ok(held) => held,
        Err(why) => return Settlement::Unreadable(format!("its copy could not be read: {why}")),
    };
    match branch_state(
        &item.old_version,
        held.as_deref(),
        &item.new_version,
        scheme_of(item),
    ) {
        // The Version alone does not say the update worked: an attempt that
        // failed, or was cut short, may leave it in the branch. Only a build
        // that succeeded settles the package; any other is continued from.
        BranchState::AlreadyUpdated => match built() {
            Ok(status) if status.is_success() => Settlement::Updated,
            Ok(_) => Settlement::Go { held },
            Err(why) => {
                Settlement::Unreadable(format!("its build results could not be read: {why}"))
            }
        },
        // `--to-version` names the target: it is not held back by a newer branch.
        BranchState::AheadOfUpstream { .. } if a.to_version.is_some() => Settlement::Go { held },
        BranchState::AheadOfUpstream { branch } => Settlement::Ahead { branch },
        BranchState::Fresh => Settlement::Go { held: None },
        BranchState::Continue => Settlement::Go { held },
    }
}

/// Whether the branch project's copy settles `item`. `true` when the item is
/// handled here, and must not be branched or built; `false` when it goes on.
pub(super) fn settled_in_branch(
    item: &mut Item,
    branch: &str,
    a: &UpdateProjectArgs,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> bool {
    let emit = Emit::run(ctx.reporter.as_ref());
    let settlement = settle(
        item,
        a,
        &|| branch_version(branch, &item.pkg, emit),
        &|| {
            let repo = crate::choose_distro(a.distro.as_deref(), branch, &a.arch);
            osc::get_build_status_confirmed(branch, &item.pkg, &repo, &a.arch)
                .map_err(|e| e.to_string())
        },
    );
    match settlement {
        Settlement::Go { held } => {
            // The build starts from the version the branch holds, which the
            // checkout is compared with (see `build_work::build_item`).
            item.held_version = held;
            false
        }
        Settlement::Unreadable(why) => {
            // The branch's copy is unknown: it is not branched over, and the
            // package is listed as skipped, not as needing the maintainer.
            let why = format!("{branch} is left alone for it: {why}");
            ctx.warn(format!("{}: {why}", item.pkg));
            report.results.push(ItemResult {
                item: item.clone(),
                outcome: ItemOutcome::Skipped(why),
                sr: SrOutcome::NotRequested,
            });
            true
        }
        Settlement::Updated => {
            ctx.say(format!(
                "{branch} already has {} at {}, built; it is not rebuilt",
                item.pkg, item.new_version
            ));
            if !a.dry_run {
                // The TUI offers a row for a package it has been told about
                // and that has finished: here, a finished build that succeeded.
                if let Some(b) = &ctx.backend {
                    b.register(item);
                }
                ctx.reporter.report(&Event::PackageFinished {
                    pkg: item.pkg.clone(),
                    version: item.new_version.clone(),
                    outcome: PackageOutcome::AlreadyBuilt,
                });
                report.results.push(ItemResult {
                    item: item.clone(),
                    outcome: ItemOutcome::AlreadyUpdated,
                    sr: SrOutcome::NotRequested,
                });
            }
            true
        }
        Settlement::Ahead { branch: held } => {
            let why = update::SkipReason::BranchAhead {
                branch: held,
                upstream: item.new_version.clone(),
            }
            .to_string();
            ctx.warn(format!("{}: {why}", item.pkg));
            // Listed under the maintainer's heading, in the dry run and in the
            // summary alike: it is not a failure and nothing is branched for it.
            report.maintainer.push(format!("{}: {why}", item.pkg));
            true
        }
    }
}

#[cfg(test)]
#[path = "existing_tests.rs"]
mod tests;
