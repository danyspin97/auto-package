//! A `%python_expand ` line inside a braced `%{python_expand ...}` block. The
//! body of a braced block is one plain shell command per line (convention 19 (section 80)),
//! and the block already runs once per flavor, so the prefix there is a
//! mistake that rpm accepts without complaint.

/// The 1-based line number and text of each line of `spec` that begins with
/// `%python_expand ` inside a braced `%{python_expand ...}` block. The opener
/// is a line that starts with `%{python_expand `; the block ends where its
/// braces balance, so a `%{...}` macro inside the opener or the body does not
/// end it early.
pub fn nested_python_expand_lines(spec: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut depth = 0;
    for (i, line) in spec.lines().enumerate() {
        let trimmed = line.trim_start();
        if depth == 0 {
            if trimmed.starts_with("%{python_expand ") {
                depth = brace_depth(trimmed, 0);
            }
            continue;
        }
        if trimmed.starts_with("%python_expand ") {
            found.push((i + 1, line.to_string()));
        }
        depth = brace_depth(line, depth);
    }
    found
}

/// The nested lines an edit adds: those of `after` whose text the spec before
/// the edit (`before`) does not already have. A line the origin's spec carries
/// is inherited and not the edit's to fix.
pub fn fresh_nested_python_expand_lines(before: &str, after: &str) -> Vec<(usize, String)> {
    let inherited: Vec<String> = nested_python_expand_lines(before)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    nested_python_expand_lines(after)
        .into_iter()
        .filter(|(_, text)| !inherited.contains(text))
        .collect()
}

/// The brace depth after `line`, starting from `depth`. `%{` opens a macro and
/// `}` closes one, so `%{$python_sitelib}` leaves the depth where it was.
pub(super) fn brace_depth(line: &str, mut depth: usize) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"%{") {
            depth += 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'}' {
            depth = depth.saturating_sub(1);
        }
        i += 1;
    }
    depth
}
