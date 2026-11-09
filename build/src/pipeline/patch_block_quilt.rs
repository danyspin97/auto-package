//! The agent's `blocked` answers read against quilt's check. A patch quilt shows
//! applying cleanly (no offset, no fuzz) is not blocked, whatever the agent
//! wrote: the block would end a package on a patch that quilt has proven fine.

use crate::agent::PatchAnswer;
use crate::patch_check::{CheckReport, PatchStatus};

/// Drops from `blocked` each patch that `report` shows applying cleanly, and
/// notes each one in `lines`.
pub(super) fn drop_blocks_quilt_clears(
    blocked: &mut Vec<PatchAnswer>,
    report: &CheckReport,
    lines: &mut Vec<String>,
) {
    blocked.retain(|b| {
        let clean = report
            .patches
            .iter()
            .any(|p| p.name == b.name && matches!(p.status, PatchStatus::Applies));
        if clean {
            lines.push(format!(
                "- {}: blocked by the agent, but quilt shows it applies cleanly: not blocked ({})",
                b.name, b.reason
            ));
        }
        !clean
    });
}
