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
