//! Which lines of a `.changes` entry belong to a flag.
//!
//! A flag is one `  FLAG: ...` line, indented under the `Update to` bullet.
//! The entry is word-wrapped, so a long flag runs on over further indented
//! lines (`wrap_changes_line` indents them under the text of the flag). A
//! reader that drops, keeps or skips flags must treat all of those lines as
//! one, so they share this classification.

/// Whether `line` opens a flag: indented, then `FLAG: `.
///
/// Known ambiguity: a flag written under a wrapped bullet follows that
/// bullet's own continuation line, so a flag start cannot be told from a
/// continuation line of an ordinary bullet that happens to begin with
/// `FLAG: `. `wrap_changes_line` never breaks a line there, so it does not
/// arise in files this tool wrote.
pub fn is_flag_start(line: &str) -> bool {
    line.starts_with("  ") && line.trim_start().starts_with("FLAG: ")
}

/// Whether `line` carries on the flag above it: indented text that is no
/// bullet and no flag of its own.
fn continues_flag(line: &str) -> bool {
    let text = line.trim_start();
    line.starts_with([' ', '\t'])
        && !text.is_empty()
        && !text.starts_with("- ")
        && !text.starts_with("* ")
        && !text.starts_with("FLAG: ")
}

/// For each of `lines`, whether it is a flag line or the continuation of one.
/// A flag ends at the next bullet, flag, blank line, unindented line or `*`
/// header.
pub fn flag_line_mask(lines: &[&str]) -> Vec<bool> {
    let mut in_flag = false;
    lines
        .iter()
        .map(|l| {
            in_flag = is_flag_start(l) || (in_flag && continues_flag(l));
            in_flag
        })
        .collect()
}
