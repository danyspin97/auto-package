//! Small helpers for the markdown of release notes and changelogs.

/// `text` with each `[text](url)` replaced by its `text`.
pub(super) fn strip_links(text: &str) -> String {
    let mut s = text.to_string();
    while let Some(open) = s.find('[') {
        let Some(mid) = s[open..].find("](").map(|i| open + i) else {
            break;
        };
        let Some(close) = s[mid..].find(')').map(|i| mid + i) else {
            break;
        };
        let text = s[open + 1..mid].to_string();
        s.replace_range(open..=close, &text);
    }
    s
}

/// `text` with the HTML comments (templates leave them in release bodies,
/// across lines too) cut out, and CRLF line ends made LF.
pub(super) fn strip_comments(text: &str) -> String {
    let mut s = text.replace("\r\n", "\n").replace('\r', "\n");
    while let Some(open) = s.find("<!--") {
        let end = s[open..].find("-->").map_or(s.len(), |i| open + i + 3);
        s.replace_range(open..end, "");
    }
    s
}

/// `text` without emoji and the marks that join them.
pub(super) fn strip_emoji(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(*c as u32,
                0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F | 0x200D)
        })
        .collect()
}

/// Whether `word` is a rule: four or more dashes, equals signs or underscores
/// and nothing else.
pub(super) fn is_rule(word: &str) -> bool {
    word.chars().count() >= 4 && word.chars().all(|c| matches!(c, '-' | '=' | '_'))
}
