//! Lines of a spec that only print. A `echo` or a `cat` of a test log in
//! `%build`, `%check` or `%install` changes no build, so it needs no `.changes`
//! bullet (see `changelog_required`).

/// The sections whose lines run while the package is built or tested.
const BUILD_SECTIONS: &[&str] = &["%build", "%check", "%install"];

/// The first words of a line that only prints.
const PRINT_WORDS: &[&str] = &[
    "echo", "printf", "cat", "tail", "head", "ls", "find", "pwd", "uname",
];

/// Tokens that make a line do more than print: a redirect, a pipe, a chain, a
/// substitution, or a `find` action that writes or deletes.
const EFFECTS: &[&str] = &[
    ">", "|", "&", ";", "`", "$(", "-delete", "-exec", "-ok", "-fprint", "-fls",
];

/// Whether `line` only prints: it starts with a print word, names no `VAR=`
/// setting, and has no redirect, pipe, chain, substitution or file action. `env`
/// prints only when given no arguments; `set` only as `set -x` or `set +x`.
fn prints_only(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    if lower == "set -x" || lower == "set +x" || lower == "env" {
        return true;
    }
    let mut words = lower.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if !PRINT_WORDS.contains(&first) || first.contains('=') {
        return false;
    }
    !lower
        .split_whitespace()
        .any(|w| w.contains('=') && !w.starts_with('-'))
        && !EFFECTS.iter().any(|e| lower.contains(e))
}

/// Whether a spec line of `section` is a build-time print.
pub(crate) fn is_build_print(section: &str, line: &str) -> bool {
    BUILD_SECTIONS
        .iter()
        .any(|s| section.split_whitespace().next() == Some(s))
        && prints_only(line)
}

#[cfg(test)]
#[path = "diagnostic_edit_tests.rs"]
mod diagnostic_edit_tests;
