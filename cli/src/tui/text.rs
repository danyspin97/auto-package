use super::*;

/// Longest prefix of `s` whose display width fits in `max_width` columns.
/// Zero-width characters (combining marks, variation selectors) and the
/// character after a zero-width joiner stay with the character before them,
/// so the cut never splits such a cluster.
pub(super) fn prefix_within_width(s: &str, max_width: usize) -> &str {
    let mut used = 0;
    let mut end = 0;
    let mut after_zwj = false;
    for (idx, c) in s.char_indices() {
        let width = c.width().unwrap_or(0);
        if width > 0 && !after_zwj {
            // `idx` starts a new cluster, so `s[..idx]` is a candidate cut.
            if used > max_width {
                return &s[..end];
            }
            end = idx;
        }
        used += width;
        after_zwj = c == '\u{200d}';
    }
    if used <= max_width { s } else { &s[..end] }
}

/// Longest suffix of `s` whose display width fits in `max_width` columns.
pub(super) fn suffix_within_width(s: &str, max_width: usize) -> &str {
    let mut used = 0;
    let mut start = s.len();
    for (idx, c) in s.char_indices().rev() {
        let width = c.width().unwrap_or(0);
        if used + width > max_width {
            break;
        }
        used += width;
        start = idx;
    }
    &s[start..]
}

/// Shortens `s` to at most `max_width` display columns, ending in "..." when cut.
pub(super) fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        s.to_string()
    } else {
        format!("{}...", prefix_within_width(s, max_width.saturating_sub(3)))
    }
}

/// Greedy word wrap to `width` display columns. Newlines in `text` stay line
/// breaks and blank lines stay blank. A word wider than `width` is broken
/// where it must be, and a glyph wider than the whole line still gets a line.
/// Returns the wrapped lines, so their count is what a viewport scrolls over.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut current = String::new();
        let mut current_w = 0;
        for word in paragraph.split_whitespace() {
            let word_w = word.width();
            if current_w > 0 && current_w + 1 + word_w <= width {
                current.push(' ');
                current.push_str(word);
                current_w += 1 + word_w;
                continue;
            }
            if current_w > 0 {
                lines.push(std::mem::take(&mut current));
            }
            let mut rest = word;
            while rest.width() > width {
                let mut head = prefix_within_width(rest, width);
                if head.is_empty() {
                    head = &rest[..rest.chars().next().map_or(0, char::len_utf8)];
                }
                lines.push(head.to_string());
                rest = &rest[head.len()..];
            }
            current.push_str(rest);
            current_w = rest.width();
        }
        // A paragraph whose last glyph was broken off whole leaves nothing
        // behind; only a blank paragraph should still produce its empty line.
        if !current.is_empty() || paragraph.trim().is_empty() {
            lines.push(current);
        }
    }
    lines
}
