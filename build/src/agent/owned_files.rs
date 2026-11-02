//! Files in the workspace that only the pipeline writes. The turn must not edit
//! them: `state.json` and the probe result steer later decisions, the locks
//! guard concurrent runs, `notes.md` is appended by the pipeline, and the OBS
//! baseline is what a failed run puts back on OBS (`obs-baseline`, see
//! `pipeline::baseline`). Its `checkout` and `files` steer that restore.

use super::response::permission_path;

/// Names in the workspace the turn may not write. `sdist.stamp` records which
/// sdist the extracted tree came from.
const PIPELINE_OWNED_FILES: [&str; 11] = [
    "state.json",
    "state.json.*",
    "*.lock",
    "build.lock",
    "flaky-probe.json",
    "flaky-probe.json.*",
    "notes.md",
    "notes.md.*",
    "sdist.stamp",
    "obs-baseline",
    "obs-baseline/**",
];

/// Denies Edit, Write and NotebookEdit on each of [`PIPELINE_OWNED_FILES`] under
/// the workspace, at its top and in any subdirectory.
pub(super) fn denied_tools(workspace: &std::path::Path) -> Vec<String> {
    let p = permission_path(workspace);
    let mut out = Vec::new();
    for tool in ["Edit", "Write", "NotebookEdit"] {
        for name in PIPELINE_OWNED_FILES {
            out.push(format!("{tool}({p}/{name})"));
            out.push(format!("{tool}({p}/**/{name})"));
        }
    }
    out
}
