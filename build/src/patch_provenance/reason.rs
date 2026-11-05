//! Why a carried patch is there (plan 2.4), for the `Description:` line and the
//! spec comment, and the text helpers those lines share. No model: the
//! `.changes` bullet that introduced the file, else the patch's own subject.

use crate::backport::subject_of_patch;
use crate::pipeline::names_in_bullet;

/// The verbs that introduce a patch in a `.changes` bullet.
const INTRODUCING: &[&str] = &["add", "added", "backport", "backported", "new"];

/// The bullets of a `.changes` text, newest first as the file has them: each
/// is the text after `- `, with its indented continuation lines joined.
pub(super) fn bullets_of(changes: &str) -> Vec<String> {
    let mut bullets: Vec<String> = Vec::new();
    let mut open = false;
    for line in changes.lines() {
        if let Some(text) = line.strip_prefix("- ") {
            bullets.push(text.to_string());
            open = true;
        } else if open && line.starts_with(char::is_whitespace) {
            if let Some(last) = bullets.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            open = false;
        }
    }
    bullets
}

/// The reason of the oldest `.changes` bullet that introduced `name`: its first
/// word is one of `INTRODUCING` and it names the file. The reason is the text
/// after `name:`, or after `name` and ` to `, ` for ` or ` - `.
pub fn reason_from_changes(changes: &str, name: &str) -> Option<String> {
    bullets_of(changes)
        .iter()
        .rev()
        .find_map(|b| reason_of_bullet(b, name))
}

fn reason_of_bullet(bullet: &str, name: &str) -> Option<String> {
    let words: Vec<&str> = bullet.split_whitespace().collect();
    let first = words.first()?.to_ascii_lowercase();
    if !INTRODUCING.contains(&first.as_str()) || !names_in_bullet(&words, name) {
        return None;
    }
    let at = bullet.find(name)?;
    let rest = &bullet[at + name.len()..];
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let reason = match rest.strip_prefix(':') {
        Some(r) => r,
        None => [" to ", " for ", " - "]
            .iter()
            .find_map(|p| rest.strip_prefix(p))?,
    };
    let reason = sanitize(reason);
    (!reason.is_empty()).then_some(reason)
}

/// The patch's subject without its `[PATCH n/m]` tags, as a reason.
pub fn reason_from_subject(text: &str) -> Option<String> {
    let mut subject = subject_of_patch(text)?;
    while subject.starts_with('[') {
        let end = subject.find(']')?;
        subject = subject[end + 1..].trim_start().to_string();
    }
    let reason = sanitize(&subject);
    (!reason.is_empty()).then_some(reason)
}

/// The reason of a patch: the `.changes` bullet, else the subject.
pub fn reason_of(changes: &str, name: &str, text: &str) -> Option<String> {
    reason_from_changes(changes, name).or_else(|| reason_from_subject(text))
}

/// Whether `c` is a control character or a bidi override or isolate, which can
/// hide text in a header.
fn is_hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `text` with hidden characters turned into spaces, whitespace collapsed, a
/// trailing `.` removed, and cut at 200 characters on a word boundary.
pub fn sanitize(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if is_hidden(c) { ' ' } else { c })
        .collect();
    let collapsed = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(collapsed.trim_end_matches('.'), 200)
}

/// `text` cut to at most `max` characters, at a word boundary when there is
/// one (a hard cut when the text is one word).
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let end = match head.rfind(' ') {
        Some(i) if i > 0 => i,
        _ => head.len(),
    };
    head[..end].trim_end().to_string()
}

/// A header field `key: value` as lines folded at 76 columns; continuation
/// lines start with one space (RFC 5322 folding). A word is never split.
pub fn fold_field(key: &str, value: &str) -> Vec<String> {
    const WIDTH: usize = 76;
    let mut out = Vec::new();
    let mut line = format!("{key}:");
    let mut empty = true;
    for word in value.split_whitespace() {
        if empty {
            line.push(' ');
        } else if line.len() + 1 + word.len() > WIDTH {
            out.push(std::mem::take(&mut line));
            line.push(' ');
        } else {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    if empty {
        return Vec::new();
    }
    out.push(line);
    out
}
