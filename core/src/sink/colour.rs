//! Colour for the terminal rendering of the ad hoc diagnostic lines (`diag!`)
//! and of the other stderr lines that look like them. It is a pure function
//! of the text and of whether colour is on, so it is tested without a
//! terminal. Callers decide the colour (see [`stderr_colour`]) and apply it to
//! the terminal copy only; the diagnostics file and the run log keep the text.
//!
//! Only SGR codes are added. Stripping them gives back the input exactly.

use owo_colors::{OwoColorize, Stream::Stderr};

/// Whether stderr takes colour: a terminal, `NO_COLOR` unset, and
/// `CLICOLOR_FORCE` honoured (owo-colors' rule for every stderr line).
pub fn stderr_colour() -> bool {
    !"".if_supports_color(Stderr, |t| t.red())
        .to_string()
        .is_empty()
}

/// Leading labels and their SGR codes (bold colour on, bold and colour off).
const LABELS: [(&str, &str); 4] = [
    ("note:", "1;36"),
    ("info:", "1;34"),
    ("warning:", "1;33"),
    ("error:", "1;31"),
];

/// Sub-step verbs at the start of a line, and their SGR codes.
const VERBS: [(&str, &str); 3] = [("searching", "2"), ("checking", "2"), ("found", "32")];

/// Phrases that mark a problem or a skipped step, wherever they occur.
const YELLOW_PHRASES: [&str; 3] = ["not used automatically", "rate-limited", "failed"];

/// `text` as the terminal shows it. With `colour` off it is returned byte for
/// byte. With it on, each line is coloured line by line (newlines are kept),
/// and a line that already has an escape sequence is left alone.
pub fn colour_diag(text: &str, colour: bool) -> String {
    if !colour {
        return text.to_string();
    }
    text.split('\n')
        .map(colour_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn colour_line(line: &str) -> String {
    if line.contains('\u{1b}') {
        return line.to_string();
    }
    let body = line.trim_start_matches(' ');
    let indent = &line[..line.len() - body.len()];
    if indent.is_empty() {
        for (label, sgr) in LABELS {
            if let Some(rest) = body.strip_prefix(label) {
                if rest.starts_with(' ') {
                    return format!("{}{}", wrap(label, sgr, "22;39"), inline(rest, None));
                }
            }
        }
    }
    for (mark, sgr) in [("✓", "32"), ("✗", "31")] {
        if body.starts_with(mark) {
            return format!("{indent}\x1b[{sgr}m{}\x1b[39m", inline(body, Some(sgr)));
        }
    }
    let mut verb_out = None;
    for (verb, sgr) in VERBS {
        if let Some(rest) = word_rest(body, verb) {
            verb_out = Some(format!(
                "{}{}",
                wrap(verb, sgr, off_for(sgr)),
                inline(rest, None)
            ));
            break;
        }
    }
    format!("{indent}{}", verb_out.unwrap_or_else(|| inline(body, None)))
}

/// The SGR code that ends `sgr` again: bold and dim end with 22, colours
/// with 39 (default foreground).
fn off_for(sgr: &str) -> &'static str {
    if sgr == "2" { "22" } else { "39" }
}

/// `rest` when `text` starts with the word `word` (followed by a space, a
/// colon or the end of the line).
fn word_rest<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(' ') | Some(':') => Some(rest),
        Some(_) => None,
    }
}

/// `text` with URLs underlined and the problem phrases in yellow. Inside a
/// coloured line (`base`), a coloured span ends by returning to that colour.
fn inline(text: &str, base: Option<&str>) -> String {
    let close_fg = base.unwrap_or("39");
    let mut out = String::with_capacity(text.len() + 16);
    let mut plain = 0;
    let mut i = 0;
    while i < text.len() {
        match span_at(text, i) {
            Some((len, Span::Url)) => {
                out.push_str(&text[plain..i]);
                out.push_str(&wrap(&text[i..i + len], "4", "24"));
                i += len;
                plain = i;
            }
            Some((len, Span::Yellow)) => {
                out.push_str(&text[plain..i]);
                out.push_str(&wrap(&text[i..i + len], "33", close_fg));
                i += len;
                plain = i;
            }
            None => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out.push_str(&text[plain..]);
    out
}

enum Span {
    Url,
    Yellow,
}

/// A URL or a problem phrase that starts at byte `i` of `text`, and its length.
fn span_at(text: &str, i: usize) -> Option<(usize, Span)> {
    let rest = &text[i..];
    let at_word_start = text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric());
    if at_word_start && (rest.starts_with("https://") || rest.starts_with("http://")) {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"']);
        return Some((url.len(), Span::Url));
    }
    if at_word_start {
        for phrase in YELLOW_PHRASES {
            if rest.starts_with(phrase) {
                let ends_word = rest[phrase.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric());
                if ends_word {
                    return Some((phrase.len(), Span::Yellow));
                }
            }
        }
    }
    None
}

/// `text` wrapped in the SGR code `sgr`, closed by the code `off`.
fn wrap(text: &str, sgr: &str, off: &str) -> String {
    format!("\x1b[{sgr}m{text}\x1b[{off}m")
}
