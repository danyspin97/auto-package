//! Colour for the terminal rendering of the ad hoc diagnostic lines (`diag!`)
//! and of the other stderr lines that look like them. It is a pure function
//! of the text and of whether colour is on, so it is tested without a
//! terminal. Callers decide the colour (see [`stderr_colour`]) and apply it to
//! the terminal copy only; the diagnostics file and the run log keep the text.
//!
//! Only SGR codes are added. Stripping them gives back the input exactly.

use owo_colors::{OwoColorize, Stream::Stderr};

use super::live::print_line;
use super::style::{Role, paint};

/// Whether stderr takes colour: a terminal, `NO_COLOR` unset, and
/// `CLICOLOR_FORCE` honoured (owo-colors' rule for every stderr line).
pub fn stderr_colour() -> bool {
    !"".if_supports_color(Stderr, |t| t.red())
        .to_string()
        .is_empty()
}

/// Prints `line` to stderr, coloured when stderr takes colour (see
/// [`colour_diag`]). Used for the terminal copy only.
pub fn eprint_coloured(line: &str) {
    print_line(&colour_diag(line, stderr_colour()));
}

/// Leading labels and their SGR codes (bold colour on, bold and colour off).
/// `error:` has the failure role instead (see [`colour_line`]).
const LABELS: [(&str, &str); 3] = [("note:", "1;36"), ("info:", "1;34"), ("warning:", "1;33")];

/// Sub-step verbs at the start of a line, and their roles.
const VERBS: [(&str, Role); 3] = [
    ("searching", Role::Dim),
    ("checking", Role::Dim),
    ("found", Role::Success),
];

/// Phrases that mark a problem or a skipped step, wherever they occur. The
/// held and interrupted phrases are the ones the end-of-run lines use.
const YELLOW_PHRASES: [&str; 5] = [
    "not used automatically",
    "rate-limited",
    "failed",
    "held while the TUI was open",
    "Interrupted",
];

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
        if let Some(rest) = body.strip_prefix("error:").filter(|r| r.starts_with(' ')) {
            return format!(
                "{}{}",
                paint("error:", Role::Failure, true),
                inline(rest, None)
            );
        }
        for (label, sgr) in LABELS {
            if let Some(rest) = body.strip_prefix(label)
                && rest.starts_with(' ')
            {
                return format!("{}{}", wrap(label, sgr, "22;39"), inline(rest, None));
            }
        }
    }
    if body.starts_with('✓') {
        return format!(
            "{indent}{}",
            paint(&inline(body, Some("32")), Role::Success, true)
        );
    }
    if body.starts_with('✗') {
        return format!("{indent}\x1b[31m{}\x1b[39m", inline(body, Some("31")));
    }
    let mut verb_out = None;
    for (verb, role) in VERBS {
        if let Some(rest) = word_rest(body, verb) {
            verb_out = Some(format!("{}{}", paint(verb, role, true), inline(rest, None)));
            break;
        }
    }
    format!("{indent}{}", verb_out.unwrap_or_else(|| inline(body, None)))
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
    let mut out = String::with_capacity(text.len() + 16);
    let mut plain = 0;
    let mut i = 0;
    while i < text.len() {
        match span_at(text, i) {
            Some((len, Span::Url)) => {
                out.push_str(&text[plain..i]);
                out.push_str(&paint(&text[i..i + len], Role::Url, true));
                i += len;
                plain = i;
            }
            Some((len, Span::Yellow)) => {
                out.push_str(&text[plain..i]);
                out.push_str(&yellow(&text[i..i + len], base));
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

/// A problem phrase: warning-coloured, or closed with the enclosing colour
/// (`base`) when it is inside a coloured line.
fn yellow(phrase: &str, base: Option<&str>) -> String {
    match base {
        None => paint(phrase, Role::Warning, true),
        Some(close) => wrap(phrase, "33", close),
    }
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
            if let Some(after) = rest.strip_prefix(phrase) {
                let ends_word = after.chars().next().is_none_or(|c| !c.is_alphanumeric());
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
