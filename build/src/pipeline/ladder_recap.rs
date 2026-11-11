//! A short recap of a spent resolution ladder, for the user. The ladder's own
//! history is long, repeats its cause round after round and quotes internal
//! rules; this keeps what a reader needs: what is wrong, and one step to take.

use ap_core::remedy::{HostDistro, Remedy, install_hint};
use ap_core::user_text::scrub_internal_refs;

use crate::agent::user_text::{ACTION_CAP, SUMMARY_CAP};

/// Most characters of one sentence of the recap, the trailing `…` included.
const SENTENCE_CAP: usize = 160;

/// What the user reads of a spent ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Recap {
    /// The cause, in one sentence.
    pub(super) headline: String,
    /// What else there is to say, one sentence each.
    pub(super) lines: Vec<String>,
    /// One step to take: an install hint of a typed remedy, else the
    /// resolver's own suggestion.
    pub(super) action: Option<String>,
}

impl Recap {
    /// The headline and the lines on one line, single-spaced.
    #[cfg(test)]
    pub(super) fn body(&self) -> String {
        self.body_within(usize::MAX, 0)
    }

    /// [`Self::body`] within `limit` characters: trailing lines are dropped
    /// whole (the first `pinned` lines never are), then the headline is cut
    /// with a trailing `…` if it still does not fit.
    pub(super) fn body_within(&self, limit: usize, pinned: usize) -> String {
        let mut lines: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        let joined = |headline: &str, lines: &[&str]| {
            std::iter::once(headline)
                .chain(lines.iter().copied())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        while lines.len() > pinned && joined(&self.headline, &lines).chars().count() > limit {
            lines.pop();
        }
        let fixed = joined("", &lines).chars().count() + 1;
        if joined(&self.headline, &lines).chars().count() > limit {
            let room = limit.saturating_sub(fixed).max(1);
            return joined(&cap(&self.headline, room), &lines);
        }
        joined(&self.headline, &lines)
    }
}

/// The recap of `history` (the first turn's reason, then each round's).
///
/// The resolver's `user_summary`, when a round gave one, is the headline as it
/// is. Without it the headline is the first sentence of the first turn's
/// reason, and when rounds followed that turn, one line counts the rounds that
/// ran. A typed `remedy` with an install hint is the action; else
/// `user_action`.
pub(super) fn recap(
    history: &[String],
    user_summary: Option<&str>,
    user_action: Option<&str>,
    remedy: Option<&Remedy>,
    host: HostDistro,
) -> Recap {
    let (headline, lines) = match user_summary
        .map(|s| clean(s, SUMMARY_CAP))
        .filter(|s| !s.is_empty())
    {
        Some(summary) => (summary, Vec::new()),
        None => from_history(history),
    };
    let action = remedy
        .and_then(|r| install_hint(&r.cause, &host))
        .or_else(|| {
            user_action
                .map(|a| clean(a, ACTION_CAP))
                .filter(|a| !a.is_empty())
        });
    Recap {
        headline,
        lines,
        action,
    }
}

/// The deterministic recap of `history`: headline and lines.
fn from_history(history: &[String]) -> (String, Vec<String>) {
    let Some(first) = history.first() else {
        return (String::new(), Vec::new());
    };
    let ran = history[1..]
        .iter()
        .filter(|r| !strip_label(r).is_empty() && !r.contains(" not run:"))
        .count();
    let lines = match ran {
        0 => Vec::new(),
        1 => vec!["1 resolution round did not fix it.".to_string()],
        n => vec![format!("{n} resolution rounds did not fix it.")],
    };
    (first_sentence(&strip_label(first)), lines)
}

/// `text` as one clean line: internal references replaced, control characters
/// dropped, whitespace collapsed, and cut to `limit` characters.
fn clean(text: &str, limit: usize) -> String {
    cap(&clean_uncapped(text), limit)
}

/// `text` cut to `limit` characters, on a character boundary, with a trailing
/// `…` when it was longer.
pub(super) fn cap(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit - 1).collect();
    out.push('…');
    out
}

/// The first sentence of `text`, cleaned: up to the first `.`, `!` or `?` that
/// ends a sentence (see [`sentence_end`]), or a CJK `。`, `！` or `？`, with
/// any closing quote or bracket right after it. A parenthesis that opens with
/// a quote (`("rustup could not ...")`) is quoted output, not part of the
/// sentence: it is left out.
pub(super) fn first_sentence(text: &str) -> String {
    let text = drop_quoted_groups(&clean_uncapped(text));
    cap(&text[..sentence_end(&text)], SENTENCE_CAP)
}

/// `text` without the `("...")` and `('...')` groups, and the space before each.
fn drop_quoted_groups(text: &str) -> String {
    let mut out = text.to_string();
    for (open, close) in [("(\"", "\")"), ("('", "')")] {
        while let Some(start) = out.find(open)
            && let Some(len) = out[start..].find(close)
        {
            let end = start + len + close.len();
            out.replace_range(start..end, "");
            let kept = out[..start].trim_end().len();
            out.replace_range(kept..start, "");
        }
    }
    out
}

/// Abbreviations that do not end a sentence, lowercase, without their last dot.
const ABBREVIATIONS: [&str; 10] = [
    "e.g", "i.e", "etc", "vs", "cf", "dr", "mr", "mrs", "ms", "approx",
];

/// Where the first sentence of `text` ends, in bytes (the text length when it
/// has no sentence end). A `.` ends it when a space or the end follows (after
/// closing quotes and brackets), unless it belongs to an ellipsis, a number or
/// version (`v1.15.0`, a file name: a letter or digit follows it), an
/// abbreviation (`e.g.`, `etc.`, `No. 5`) or a single capital (`J.`).
fn sentence_end(text: &str) -> usize {
    let closers = |c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '»');
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let mut end = i + c.len_utf8();
        match c {
            '。' | '！' | '？' => {}
            '.' | '!' | '?' => {
                let rest = &text[end..];
                let after = rest.trim_start_matches(closers);
                if !(after.is_empty() || after.starts_with(char::is_whitespace)) {
                    continue;
                }
                if c == '.' && (text[..i].ends_with('.') || rest.starts_with('.')) {
                    // An ellipsis ends nothing, unless it ends the text.
                    if !after.is_empty() {
                        continue;
                    }
                }
                if c == '.' && !after.is_empty() && abbreviation_before(text, i, after.trim_start())
                {
                    continue;
                }
                end += rest.len() - after.len();
            }
            _ => continue,
        }
        return end;
    }
    text.len()
}

/// Whether the `.` at byte `dot` ends an abbreviation; `next` is the text
/// after it.
fn abbreviation_before(text: &str, dot: usize, next: &str) -> bool {
    let word = text[..dot]
        .rsplit(|c: char| c.is_whitespace() || matches!(c, '(' | '"' | '\'' | '[' | ','))
        .next()
        .unwrap_or("");
    let lower = word.to_lowercase();
    ABBREVIATIONS.contains(&lower.as_str())
        || (lower == "no" && next.starts_with(|c: char| c.is_ascii_digit()))
        || (word.chars().count() == 1 && word.starts_with(char::is_uppercase))
}

/// [`clean`] without the cap, so a sentence is found before it is cut.
fn clean_uncapped(text: &str) -> String {
    let scrubbed = scrub_internal_refs(text);
    let printable: String = scrubbed
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();
    printable.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` without the labels the ladder gave it: `turn 0:`, `round 2:`,
/// `round 1 answered no_change:`, `round 1 edit rejected:`, `round 2 not run:`,
/// one after the other (a stored history labels each entry twice).
fn strip_label(text: &str) -> String {
    let mut text = text.trim();
    while let Some(body) = strip_one_label(text) {
        text = body;
    }
    text.to_string()
}

fn strip_one_label(text: &str) -> Option<&str> {
    let rest = text
        .strip_prefix("turn ")
        .or_else(|| text.strip_prefix("round "))?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &rest[digits..];
    [":", " answered no_change:", " edit rejected:", " not run:"]
        .iter()
        .find_map(|label| rest.strip_prefix(label))
        .map(str::trim)
}
