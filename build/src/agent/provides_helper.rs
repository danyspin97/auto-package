//! The package-lookup helper of a turn: the Bash grant that runs it and what
//! the turn is told about it. The command is this binary's own path, as for
//! the byte-edit helper, and the denies of [`edit_bytes_denies`] (path,
//! metacharacter and quote rules for the binary's path) cover it too. The words
//! are checked by the helper itself.

use super::*;

/// The sub-command of this binary that looks a package up.
const PROVIDES_SUBCOMMAND: &str = "provides";

/// Whether a turn of `task` has the lookup: the turns that edit a package, the
/// ones that were granted `zypper` queries.
fn has_helper(task: Task) -> bool {
    !matches!(task, Task::TranslateUnresolvableDeps | Task::RequestReview)
}

/// Whether a turn of `task` is granted the lookup call: it has the lookup and
/// this binary's path can be spelled in a permission rule.
pub(crate) fn lookup_granted(task: Task) -> bool {
    edit_bytes_command().is_some() && has_helper(task)
}

/// The grant of the lookup: the one command shape, for a turn that edits.
pub(super) fn provides_grant(task: Task, command: Option<&str>) -> String {
    match command {
        Some(cmd) if has_helper(task) => format!(",Bash({cmd} {PROVIDES_SUBCOMMAND} *)"),
        _ => String::new(),
    }
}

/// What a turn is told about the lookup, after its task text: how to ask which
/// package provides a name, since a name with parentheses is refused when typed
/// as a `zypper` query.
pub(super) fn provides_addendum(task: Task) -> String {
    provides_text(task, edit_bytes_command().as_deref())
}

pub(super) fn provides_text(task: Task, command: Option<&str>) -> String {
    let Some(cmd) = command.filter(|_| has_helper(task)) else {
        return String::new();
    };
    format!(
        "\nTo find the package that provides a name for a BuildRequires, run the lookup as one \
         plain call (no quotes, `;`, `|`, `$`, parentheses):\n\
         - `{cmd} {PROVIDES_SUBCOMMAND} pkgconfig <module>` for `Package <module> was not found` \
           from pkg-config (answers for `pkgconfig(<module>)`)\n\
         - `{cmd} {PROVIDES_SUBCOMMAND} lib <name>` for `unable to find library -l<name>` or \
           `cannot find -l<name>` (it asks for `pkgconfig(<name>)` only)\n\
         - `{cmd} {PROVIDES_SUBCOMMAND} cmake <Package>` for a CMake `Findxxx`/`xxxConfig.cmake` \
           error, and `{cmd} {PROVIDES_SUBCOMMAND} file usr/bin/<tool>` for a tool.\n\
         `file` is reliable only for a tool or file in package metadata: ask headers and \
         libraries through `pkgconfig`, `cmake` or `lib`, not `file`.\n\
         It reads the repository data of this machine, a hint that can differ from the \
         project's targets; it needs no network. A refused `zypper` command with \
         `pkgconfig(...)` is answered by this call: do not try other spellings of the query. \
         Prefer a name the lookup returned. When it finds nothing or cannot run, keep a name \
         only with a comment naming the source file and line that needs it, and mark it \
         `unverified`; do not drop a plausible BuildRequires and do not guess silently.\n"
    )
}
