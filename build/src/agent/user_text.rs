//! The optional `user_summary` and `user_action` of a turn's reply: one or two
//! plain sentences for the package's maintainer, and one step the maintainer can
//! take. They are normalised as the reply is read, so whatever shows them later
//! gets a short, single-line value or none.

use serde::{Deserialize, Deserializer};

/// Most characters of a `user_summary`, the trailing `…` included.
pub(crate) const SUMMARY_CAP: usize = 300;

/// Most characters of a `user_action`, the trailing `…` included.
pub(crate) const ACTION_CAP: usize = 200;

/// A value with more lines than this, before normalisation, is a log paste.
const MAX_LINES: usize = 3;

/// A line break for the line count: LF, a lone CR (a CRLF is folded to one LF
/// first), U+2028 and U+2029.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Characters that change how a value displays, or hide in it: bidirectional
/// controls, zero-width characters and the byte order mark. A value holding one
/// is dropped rather than stripped, as a value with a control character is: the
/// text cannot be trusted to read as it was written.
fn is_spoofing(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'
            | '\u{FEFF}'
    )
}

/// `raw` as the user reads it: trimmed, with its whitespace (line breaks
/// included) collapsed to single spaces, and cut to `cap` characters with a
/// trailing `…` when it is longer. `None` when `raw` is empty, when it has more
/// than [`MAX_LINES`] lines, or when it holds a control character other than a
/// line break or a character from [`is_spoofing`]: such a value is a log paste
/// or a disguised one, and is dropped rather than shown.
pub(crate) fn normalise_user_text(raw: &str, cap: usize) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.replace("\r\n", "\n").split(is_line_break).count() > MAX_LINES {
        return None;
    }
    if raw
        .chars()
        .any(|c| (c.is_control() && !is_line_break(c)) || is_spoofing(c))
    {
        return None;
    }
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= cap {
        return Some(text);
    }
    let cut: String = text.chars().take(cap.saturating_sub(1)).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// A reply's text field, normalised as it is deserialised. Anything that is not
/// a string (a number, a list, an object, a bool) reads as `None`: the field is
/// cosmetic, so it must not fail the reply, which carries the action that matters.
fn text_field<'de, D: Deserializer<'de>>(d: D, cap: usize) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value.as_str().and_then(|s| normalise_user_text(s, cap)))
}

/// The `user_summary` field of a reply, normalised as it is deserialised.
pub(super) fn summary_field<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    text_field(d, SUMMARY_CAP)
}

/// The `user_action` field of a reply, normalised as it is deserialised.
pub(super) fn action_field<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    text_field(d, ACTION_CAP)
}

/// The paragraph a resolution round's context ends with: it asks for the two
/// fields above. It is given to the rounds only, never to another turn.
pub(crate) const LADDER_SUMMARY_TEXT: &str = r#"Also reply with "user_summary" (at most two sentences, plain words for the package maintainer: what is wrong) and "user_action" (one concrete step the maintainer can take, such as a command, or an empty string if there is none). Do not mention convention numbers, section numbers or file paths of the tool, and do not paste logs."#;
