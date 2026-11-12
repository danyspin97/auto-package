//! Colour for the end-of-run summary that `main` prints on stdout. The summary
//! is built as plain text first; this colours it line by line for the
//! terminal, and only when stdout takes colour (see [`stdout_colour`]). It is
//! a pure function of the text and of whether colour is on, so it is tested
//! without a terminal. Files, JSON and the run log never get it.
//!
//! Only SGR codes are added. Stripping them gives back the input exactly.

use super::style::{Role, paint};

/// Section titles whose rows are not packages, so their first word is not
/// painted as one.
const NOT_PACKAGE_SECTIONS: [&str; 1] = ["Hosts refused this run"];

/// The marks that start a row and the role of the whole row.
const MARKS: [(char, Role); 3] = [('✓', Role::Success), ('✗', Role::Failure), ('⇥', Role::Dim)];

/// The two arrows of a version change.
const ARROWS: [&str; 2] = ["->", "→"];

/// The section of the tries block: its rows are [`tries_row`]s, and its
/// bucket line of counts is left plain.
const TRIES_SECTION: &str = "Tries to build";

/// The section whose rows are a `risk` label and its text.
const REVIEW_SECTION: &str = "Needs review";

/// The label that starts a review row, and its role.
const RISK_LABEL: &str = "risk";

/// `text` as the terminal shows it. With `colour` off it is returned byte for
/// byte. With it on, each line is coloured alone (newlines are kept), and a
/// line that already has an escape sequence is left alone.
pub fn colour_summary(text: &str, colour: bool) -> String {
    if !colour {
        return text.to_string();
    }
    let mut section = "";
    text.split('\n')
        .map(|line| {
            if let Some(title) = heading_title(line) {
                section = title;
                return paint(title, Role::Heading, true) + &line[title.len()..];
            }
            summary_line(line, section)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The title of a `Title (n):` heading line, or `None` for any other line.
fn heading_title(line: &str) -> Option<&str> {
    let head = line.strip_suffix("):")?;
    let open = head.rfind(" (")?;
    let count = &head[open + 2..];
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let title = &line[..open];
    (!title.is_empty() && !title.starts_with(' ')).then_some(title)
}

fn summary_line(line: &str, section: &str) -> String {
    if line.contains('\u{1b}') {
        return line.to_string();
    }
    let body = line.trim_start_matches(' ');
    let indent = &line[..line.len() - body.len()];
    if let Some((_, role)) = MARKS.iter().find(|(mark, _)| body.starts_with(*mark)) {
        return format!("{indent}{}", paint(body, *role, true));
    }
    if section == TRIES_SECTION {
        if indent == "    "
            && let Some(row) = tries_row(body)
        {
            return format!("{indent}{row}");
        }
        // The bucket line of counts (`1 try: 2, 3+ tries: 1`) names no package.
        if indent == "  " {
            return line.to_string();
        }
    }
    if section == REVIEW_SECTION
        && indent == "    "
        && let Some(text) = body
            .strip_prefix(RISK_LABEL)
            .and_then(|t| t.strip_prefix("  "))
    {
        return format!(
            "{indent}{}  {}",
            paint(RISK_LABEL, Role::Warning, true),
            inline(text)
        );
    }
    let body = if indent == "  " && !NOT_PACKAGE_SECTIONS.contains(&section) {
        paint_first_word(body)
    } else {
        body.to_string()
    };
    format!("{indent}{}", inline(&body))
}

/// A row of the tries section, `name  N tries, built · last failed try: why`
/// (or `not built [shape] · why`, `no result`): the name bold, the outcome
/// green or red, the reason dim and the rest plain. `None` when the text is
/// not such a row. The padding after the name is kept as it is.
fn tries_row(body: &str) -> Option<String> {
    let name_end = body.find(' ')?;
    let name = &body[..name_end];
    if !is_package_name(name) {
        return None;
    }
    let rest = &body[name_end..];
    let detail = rest.trim_start_matches(' ');
    let gap = &rest[..rest.len() - detail.len()];
    let (head, reason) = match detail.split_once(" · ") {
        Some((head, reason)) => (head, Some(reason)),
        None => (detail, None),
    };
    let (count, outcome) = head.split_once(", ")?;
    let is_count = count.starts_with(|c: char| c.is_ascii_digit())
        && (count.ends_with(" try") || count.ends_with(" tries"));
    if !is_count {
        return None;
    }
    let outcome = if let Some(shape) = outcome.strip_prefix("not built") {
        format!("{}{shape}", paint("not built", Role::Failure, true))
    } else if let Some(rest) = outcome.strip_prefix("no result") {
        format!("{}{rest}", paint("no result", Role::Failure, true))
    } else if let Some(rest) = outcome.strip_prefix("built") {
        format!("{}{rest}", paint("built", Role::Success, true))
    } else {
        outcome.to_string()
    };
    let reason = match reason {
        None => String::new(),
        Some(text) => {
            let (lead, why) = match text.strip_prefix("last failed try: ") {
                Some(why) => ("last failed try: ", why),
                None => ("", text),
            };
            let why = if why.is_empty() {
                String::new()
            } else {
                paint(why, Role::Dim, true)
            };
            format!(" · {lead}{why}")
        }
    };
    Some(format!(
        "{}{gap}{count}, {outcome}{reason}",
        paint(name, Role::Package, true)
    ))
}

/// Whether `name` looks like a package name: letters, digits and `._+~-`,
/// starting with a letter or digit.
fn is_package_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '+' | '~' | '-'))
}

/// `body` with its first word painted as a package name, when it looks like
/// one. A colon after the name is left plain: `pkg: reason`.
fn paint_first_word(body: &str) -> String {
    let word_end = body.find(' ').unwrap_or(body.len());
    let word = &body[..word_end];
    let name = word.strip_suffix(':').unwrap_or(word);
    if !is_package_name(name) {
        return body.to_string();
    }
    format!(
        "{}{}",
        paint(name, Role::Package, true),
        &body[name.len()..]
    )
}

/// `text` with each version change `a -> b` (or `a → b`) coloured, and each
/// URL underlined. Words are the runs between whitespace, so padding (several
/// spaces before an arrow) still makes a change; the text between words is
/// copied as it is.
fn inline(text: &str) -> String {
    let words = word_spans(text);
    let mut out = String::with_capacity(text.len() + 16);
    let mut plain = 0;
    let mut i = 0;
    while i < words.len() {
        let (start, end) = words[i];
        let is_change =
            i + 2 < words.len() && ARROWS.contains(&&text[words[i + 1].0..words[i + 1].1]);
        if is_change {
            let (new_start, new_end) = words[i + 2];
            out.push_str(&text[plain..start]);
            out.push_str(&paint(&text[start..end], Role::VersionOld, true));
            out.push_str(&text[end..new_start]);
            out.push_str(&paint(&text[new_start..new_end], Role::VersionNew, true));
            plain = new_end;
            i += 3;
        } else {
            out.push_str(&text[plain..start]);
            out.push_str(&url_word(&text[start..end]));
            plain = end;
            i += 1;
        }
    }
    out.push_str(&text[plain..]);
    out
}

/// The byte ranges of the runs of non-whitespace in `text`.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(s)) => {
                spans.push((s, at));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}

/// `word`, with an URL in it underlined. Leading brackets and quotes stay
/// plain, and so does the punctuation after the URL.
fn url_word(word: &str) -> String {
    let lead = word.len() - word.trim_start_matches(['(', '[', '"', '\'']).len();
    let rest = &word[lead..];
    if !(rest.starts_with("https://") || rest.starts_with("http://")) {
        return word.to_string();
    }
    let url = rest.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"']);
    format!(
        "{}{}{}",
        &word[..lead],
        paint(url, Role::Url, true),
        &rest[url.len()..]
    )
}
