//! The byte-edit helper of a RefreshPatches turn: the Bash grant that runs it,
//! and the denies that keep the command to its one shape. The command is this
//! binary's own path, so the grant names the program that runs the turn; a
//! path that a permission rule cannot spell (a space, a glob character) gets no
//! grant at all.

use super::*;

/// What a RefreshPatches turn is told about the byte-edit helper, after its
/// task text. Empty for every other task.
pub(super) fn edit_bytes_addendum(task: Task) -> String {
    if task != Task::RefreshPatches {
        return String::new();
    }
    refresh_helper_text(edit_bytes_command().as_deref())
}

/// The paragraph of the refresh prompt that names the helper's one call, or
/// says there is none in this run.
pub(super) fn refresh_helper_text(command: Option<&str>) -> String {
    let Some(cmd) = command else {
        return "\nA file that Edit and Write refuse as not UTF-8 cannot be edited in this \
                run (no byte-edit helper is available): answer that patch as `blocked`, \
                naming the file.\n"
            .to_string();
    };
    format!(
        "\nA file that Edit and Write refuse as not UTF-8 is edited with the byte-edit \
         helper, the one command for it, run from the workspace:\n\
         1. Write the text to replace, copied byte for byte from the file, to \
            `{EDIT_BYTES_SCRATCH_DIR}/old.txt`, and its replacement to \
            `{EDIT_BYTES_SCRATCH_DIR}/new.txt`, with the Write tool. Both files hold UTF-8 \
            text, so a byte that is not ASCII in the file cannot be written here: choose an \
            ASCII fragment, and make the old text occur exactly once in the file.\n\
         2. `{cmd} {EDIT_BYTES_SUBCOMMAND} source/<path> --old-file \
            {EDIT_BYTES_SCRATCH_DIR}/old.txt --new-file {EDIT_BYTES_SCRATCH_DIR}/new.txt`, \
            where `<path>` is the file under `source/`. It replaces that one occurrence and \
            keeps every other byte, line endings and encoding included.\n\
         3. Then `cd source && quilt refresh -p ab --no-timestamps --no-index` (`-p 0` for a \
            `-p0` patch), as for any other hunk.\n\
         Use it for one edit per patch cycle, as with Edit. If it reports zero or several \
         matches, or refuses the path, answer that patch `blocked` with the reason it gives. \
         Give the command as one plain call: no quotes, globs, `&&`, `;`, `|` or \
         redirection, which the sandbox refuses.\n"
    )
}

/// The directory, under the turn's workspace, where the turn writes the old and
/// the new text of an edit.
const EDIT_BYTES_SCRATCH_DIR: &str = "byte-edit";

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
