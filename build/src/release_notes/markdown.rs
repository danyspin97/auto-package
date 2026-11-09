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
