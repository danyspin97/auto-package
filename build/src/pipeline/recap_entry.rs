//! The upstream recap of a version update's `.changes` entry, and the
//! `NoUpstreamNotes` risk flag that records whether the notes were found.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::prompt_notes::*;

/// Adds the upstream recap to the update's `.changes` entry, then settles the
/// `NoUpstreamNotes` flag from whether the recap found no notes.
pub(super) fn add_update_recap(ctx: &RunCtx, old_version: Option<&str>) {
    let RunCtx {
        config,
        reporter,
        ws,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let recap = crate::release_notes::add_recap(
        checkout_dir,
        config.source_repo.as_deref(),
        old_version,
        &config.version,
    );
    if let Ok(missing) = &recap {
        update_state(ws, reporter, |st| {
            crate::risk_flags::settle_no_upstream_notes(
                st,
                &config.run_id,
                &config.version,
                *missing,
            )
        });
    }
    warn_if_err(
        reporter,
        "adding the upstream recap to the update .changes entry",
        recap.map(|_| ()),
    );
}
