//! Reads `setup_requires` from a setup.cfg as text. Nothing is executed.

/// The `setup_requires` entries of a setup.cfg `[options]` section, in order.
/// A value that is not a plain list (`file:`, `attr:`) is skipped, and so is
/// a key with no value. A value on one line is split on commas; a value that
/// continues on indented lines is split on line breaks only, as setuptools does.
pub(super) fn setup_cfg_setup_requires(text: &str) -> Vec<String> {
    let mut in_options = false;
    // The key being read: its value on the key line, and its continuation lines.
    let mut pending: Option<(&str, Vec<&str>)> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // An indented line continues the key above it; it never starts a key.
        if line.starts_with([' ', '\t']) {
            if let Some((_, continued)) = pending.as_mut()
                && !is_comment(trimmed)
            {
                continued.push(trimmed);
            }
            continue;
        }
        flush(pending.take(), &mut out);
        if let Some(section) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_options = section.trim() == "options";
            continue;
        }
        if !in_options || trimmed.is_empty() || is_comment(trimmed) {
            continue;
        }
        let Some(split) = trimmed.find(['=', ':']) else {
            continue;
        };
        if trimmed[..split].trim() != "setup_requires" {
            continue;
        }
        let value = trimmed[split + 1..].trim();
        if value.starts_with("file:") || value.starts_with("attr:") {
            continue;
        }
        pending = Some((value, Vec::new()));
    }
    flush(pending.take(), &mut out);
    out
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with(';')
}

fn flush(pending: Option<(&str, Vec<&str>)>, out: &mut Vec<String>) {
    let Some((first, continued)) = pending else {
        return;
    };
    if continued.is_empty() {
        // One line: the entries are comma separated.
        out.extend(
            first
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    } else {
        // Several lines: each line is one entry, and a comma belongs to it.
        out.extend(
            std::iter::once(first)
                .chain(continued)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
}

/// The `setup_requires` of a setup.py, read as text. Only a literal list of
/// plain quoted strings counts: `setup_requires=["wheel", "cython>=3"]`. A
/// name, a call, a concatenation, an f-string, a comment inside the list, or
/// a second `setup_requires =` in the file gives nothing, since the value
/// would have to be run to be known.
pub(super) fn setup_py_setup_requires(text: &str) -> Vec<String> {
    let mut assignments = keyword_assignments(text);
    let (Some(start), None) = (assignments.next(), assignments.next()) else {
        return Vec::new();
    };
    literal_string_list(&text[start..]).unwrap_or_default()
}

/// Byte offsets just after each `setup_requires =` (not `==`) in `text`,
/// where the name is a whole identifier. A match after a `#` on its line (a
/// comment) or after a `.` (an attribute such as `self.setup_requires`) is
/// not the keyword argument, so it is left out.
fn keyword_assignments(text: &str) -> impl Iterator<Item = usize> + '_ {
    const KEYWORD: &str = "setup_requires";
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(KEYWORD).filter_map(move |(at, _)| {
        let before = &text[..at];
        let before_ok = before.chars().next_back().is_none_or(|c| !is_ident(c));
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let in_comment = before[line_start..].contains('#');
        let attribute = before.trim_end().ends_with('.');
        let after = &text[at + KEYWORD.len()..];
        let after_ok = after.chars().next().is_none_or(|c| !is_ident(c));
        let rest = after.trim_start();
        let eq = rest.strip_prefix('=')?;
        let counts = before_ok && after_ok && !in_comment && !attribute;
        (counts && !eq.starts_with('=')).then(|| text.len() - eq.len())
    })
}

/// The strings of a list literal that starts (after blanks) with `[`, or
/// `None` when anything else appears before its `]`.
fn literal_string_list(text: &str) -> Option<Vec<String>> {
    let mut rest = text.trim_start().strip_prefix('[')?.trim_start();
    let mut out = Vec::new();
    loop {
        if let Some(after) = rest.strip_prefix(']') {
            // The list is the whole argument: nothing may follow it but the
            // end of the call (`['a'] + extra` is not a literal).
            let after = after.trim_start();
            return (after.is_empty() || after.starts_with([',', ')'])).then_some(out);
        }
        let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
        let body = &rest[1..];
        let end = body.find(quote)?;
        let value = &body[..end];
        if value.contains(['\\', '\n', '\r']) {
            return None;
        }
        out.push(value.to_string());
        rest = body[end + 1..].trim_start();
        match rest.chars().next()? {
            ',' => rest = rest[1..].trim_start(),
            ']' => {}
            _ => return None,
        }
    }
}
