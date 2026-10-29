//! The byte-edit helper of a RefreshPatches turn: the Bash grant that runs it,
//! and the denies that keep the command to its one shape. The command is this
//! binary's own path, so the grant names the program that runs the turn; a
//! path that a permission rule cannot spell (a space, a glob character) gets no
//! grant at all.

use super::*;

/// The sub-command of this binary that performs one byte edit.
const EDIT_BYTES_SUBCOMMAND: &str = "edit-bytes";

/// This binary's path as a permission rule can spell it, or `None`: an
/// absolute path of letters, digits and `/._-+@:,=%` only.
pub(super) fn edit_bytes_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    spellable_command(exe.to_str()?)
}

/// `path` when a permission rule can spell it (see [`edit_bytes_command`]).
pub(super) fn spellable_command(path: &str) -> Option<String> {
    let spellable = path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@:,=%".contains(c));
    spellable.then(|| path.to_string())
}

/// The grant of the helper: the one command shape, for a RefreshPatches turn
/// only. The path arguments are checked by the helper itself.
pub(super) fn edit_bytes_grant(task: Task, command: Option<&str>) -> String {
    match (task, command) {
        (Task::RefreshPatches, Some(cmd)) => {
            format!(",Bash({cmd} {EDIT_BYTES_SUBCOMMAND} *)")
        }
        _ => String::new(),
    }
}

/// Denies for the helper's command, for every turn: the path, quote and
/// metacharacter rules that quilt's commands have (see
/// [`path_and_metachar_denies`]), and the quote characters, which the helper
/// has no use for.
pub(super) fn edit_bytes_denies(workspace: &std::path::Path, command: Option<&str>) -> Vec<String> {
    let Some(cmd) = command else {
        return Vec::new();
    };
    let mut out = path_and_metachar_denies(workspace, cmd);
    out.push(format!("Bash({cmd} *\"*)"));
    out.push(format!("Bash({cmd} *'*)"));
    out
}
