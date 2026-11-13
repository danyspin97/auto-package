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

/// The `package` link lists the checkout's files to the turn: its line is in the
/// workspace map only when the link exists.
#[test]
fn the_workspace_map_lists_the_package_link_when_it_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let line = "\n  package/";
    let prompt = build_turn_prompt(&request(ws, &[]));
    assert!(
        !prompt.contains(line),
        "no link, yet the map lists package/"
    );
    let checkout = ws.join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    std::os::unix::fs::symlink(&checkout, ws.join("package")).unwrap();
    let prompt = build_turn_prompt(&request(ws, &[]));
    assert!(prompt.contains(line), "the map lacks the package/ line");
    assert!(prompt.contains("Edit only the spec at its absolute path"));
}
