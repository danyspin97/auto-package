//! The workspace section of a turn's prompt: the working directory is the
//! workspace, and every path a command gives is relative to it.

use super::prompt::request;
use crate::agent::build_prompt::build_turn_prompt;

/// The agent guessed absolute paths and `..` without this, and the sandbox
/// refused them.
#[test]
fn the_workspace_section_makes_paths_relative_to_the_workspace() {
    let ws = std::path::Path::new("/nonexistent-workspace");
    let prompt = build_turn_prompt(&request(ws, &[]));
    let at = prompt
        .find("## Workspace\n")
        .expect("the workspace section is present");
    let section = &prompt[at..];
    for want in [
        "Your working directory is this workspace.",
        "relative to it",
        "`conventions/75.md`",
        "never an absolute path or `..`",
    ] {
        assert!(section.contains(want), "the workspace section lacks {want}");
    }
}
