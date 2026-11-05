//! The origin's spec, read before the package is branched. An origin whose spec
//! is missing or whose `_link` is broken gives a branch the update cannot work
//! from: it is reported and not branched. A read that failed may pass, so the
//! package is left alone with that cause, and is not called broken.

use super::*;

/// The skip for an origin whose spec read failed, or `None` when the spec was
/// read. The reason is the read's own, the same one the plan gives a package.
pub(super) fn unreadable_origin(
    read: &Result<String, update::ReadError>,
) -> Option<update::SkipReason> {
    read.as_ref()
        .err()
        .map(|e| update::SkipReason::from(e.clone()))
}

/// The skip an origin's read gives when the origin is not broken, or `None`.
/// OBS's 404 for the origin: the package is not on OBS. An `_aggregate` and a
/// package with no files each have their own text.
pub(super) fn own_skip(origin: &str, read: &Result<String, update::ReadError>) -> Option<String> {
    match read {
        Err(update::ReadError::Absent(why)) => {
            Some(format!("the origin {origin} is not on OBS: {why}"))
        }
        Err(e @ (update::ReadError::Aggregate(_) | update::ReadError::NoFiles)) => {
            Some(e.to_string())
        }
        _ => None,
    }
}

/// Whether a failed read may pass on a retry: OBS did not answer, rather than
/// the origin being broken or absent.
pub(super) fn is_transient(read: &Result<String, update::ReadError>) -> bool {
    matches!(read, Err(update::ReadError::Failed(_)))
}

/// Whether `item`'s origin cannot be read, so that `item` is left out. A broken
/// origin (no spec, a broken `_link`) is warned about and listed under the
/// maintainer's heading, in the dry run and in the summary alike. A failed read
/// is warned about and listed as skipped, since it may pass on a retry. Either
/// way nothing is branched for it.
pub(super) fn settled_as_broken_origin(item: &Item, ctx: &Ctx<'_>, report: &mut Report) -> bool {
    let emit = Emit::run(ctx.reporter.as_ref());
    // The plan keeps the spec it read for a package it found: that read is
    // reused, and otherwise the spec is read now.
    let read = update::planned_spec(&item.origin, &item.pkg, emit);
    let Some(reason) = unreadable_origin(&read) else {
        return false;
    };
    // An origin OBS says is not there, an _aggregate, or one with no files is
    // not broken: it is skipped with its own reason.
    if let Some(why) = own_skip(&item.origin, &read) {
        ctx.warn(format!("{}: {why}", item.pkg));
        report.results.push(ItemResult {
            item: item.clone(),
            outcome: ItemOutcome::Skipped(why),
            sr: SrOutcome::NotRequested,
        });
        return true;
    }
    if is_transient(&read) {
        let why = format!(
            "the origin {} could not be read, so it is left alone: {reason}",
            item.origin
        );
        ctx.warn(format!("{}: {why}", item.pkg));
        report.results.push(ItemResult {
            item: item.clone(),
            outcome: ItemOutcome::Skipped(why),
            sr: SrOutcome::NotRequested,
        });
        return true;
    }
    let why = format!("the origin {} is broken: {reason}", item.origin);
    ctx.warn(format!("{}: {why}", item.pkg));
    report.maintainer.push(format!("{}: {why}", item.pkg));
    true
}

#[cfg(test)]
#[path = "origin_tests.rs"]
mod tests;
