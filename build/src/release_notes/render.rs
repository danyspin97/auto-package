//! Turning the notes of releases into the lines of a `.changes` recap: the
//! bullets of the notes, cleaned, or the link-only form when they are too many.

use super::caps::{BULLET_CHARS, cut_bullet, head, is_security};
use super::markdown::{is_rule, strip_comments, strip_emoji, strip_links};
use super::routine::is_routine;
use crate::sources::RepoRelease;

/// The most bullets a recap lists as they are; more are summarized.
pub(super) const MAX_BULLETS: usize = 10;
/// The most links the "too many changes" form lists.
const MAX_LINKS: usize = 5;
/// Width of a `.changes` line, including its two-space indent.
const WIDTH: usize = 67;
/// A bullet's source is read up to this many characters.
const MAX_SOURCE_CHARS: usize = 20_000;
/// The most lines a recap takes. A recap whose bullets wrap to more lines is
/// not listed: it falls back to the link form, as too many bullets do.
pub(super) const MAX_LINES: usize = 40;

/// What the notes of the releases between two versions come to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Notes {
    /// Short bullet notes, ready to list.
    Bullets(Vec<String>),
    /// Notes too long or not in bullets, for a summary to condense.
    Long(String),
    /// No release has notes.
    None,
}

/// Sorts the notes of `releases` (newest first) into the form they take.
pub(super) fn classify(releases: &[RepoRelease]) -> Notes {
    let with_notes: Vec<&RepoRelease> = releases
        .iter()
        .filter(|r| !strip_comments(&r.body).trim().is_empty())
        .collect();
    if with_notes.is_empty() {
        return Notes::None;
    }
    let every_release_has_bullets = with_notes.iter().all(|r| !bullets_of(&r.body).is_empty());
    // Routine bullets (CI, bumps) are left out before the count: a release
    // that has only those has nothing to list.
    let bullets: Vec<String> = with_notes
        .iter()
        .flat_map(|r| bullets_of(&r.body))
        .filter(|b| !is_routine(b))
        .collect();
    if every_release_has_bullets {
        if bullets.is_empty() {
            return Notes::None;
        }
        if bullets.len() <= MAX_BULLETS {
            let cut: Vec<String> = bullets
                .iter()
                .map(|b| cut_bullet(b, BULLET_CHARS))
                .collect();
            if fits_lines(&cut) {
                return Notes::Bullets(cut);
            }
        }
    }
    let text: String = with_notes
        .iter()
        .map(|r| format!("## {}\n{}\n", r.tag, strip_comments(&r.body).trim()))
        .collect();
    Notes::Long(head(&text, 20_000))
}

/// The bullets of release notes in markdown, one line each, without the
/// markup, the "by @user in <pull request>" tails of generated notes, and the
/// contributor and full-changelog sections they end with. A bullet wrapped
/// onto indented lines below it is one bullet: its lines are joined.
pub(super) fn bullets_of(body: &str) -> Vec<String> {
    let mut skip = false;
    let mut items: Vec<String> = Vec::new();
    // Whether the last line was a bullet's, so that an indented line after it
    // continues that bullet.
    let mut open = false;
    let body = strip_comments(body);
    for raw in body.lines() {
        let line = raw.trim();
        if let Some(head) = line.strip_prefix('#') {
            let head = head.trim_start_matches('#').to_ascii_lowercase();
            skip = head.contains("contributor") || head.contains("full changelog");
            open = false;
            continue;
        }
        if skip || line.to_ascii_lowercase().contains("full changelog") {
            open = false;
            continue;
        }
        if let Some(item) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| line.strip_prefix(m))
            .map(str::trim)
        {
            items.push(item.to_string());
            open = true;
        } else if open && raw.starts_with([' ', '\t']) && !line.is_empty() {
            if let Some(last) = items.last_mut() {
                last.push(' ');
                last.push_str(line);
            }
        } else {
            open = false;
        }
    }
    items
        .iter()
        .map(|item| {
            let text = tidy(item);
            strip_credits(&text, is_security(&text))
        })
        .filter(|item| item.chars().any(char::is_alphanumeric))
        .collect()
}

/// `text` without the contributor credits (`by @user`, `(@user)`, `(#NN by
/// Name)`, a `by Name` after a PR reference, `See PR #NN (Name).`) and, unless
/// `keep_refs` (a security line, whose numbers name the fix), without the
/// issue and pull request numbers. Its words are single-spaced.
pub(super) fn strip_credits(text: &str, keep_refs: bool) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let lower = w.to_ascii_lowercase();
        // `by @user`, with the mark that ends a sentence kept.
        if lower == "by" && words.get(i + 1).is_some_and(|n| n.starts_with('@')) {
            push_trailing(&mut out, words[i + 1]);
            i += 2;
            continue;
        }
        if w.starts_with("(@") && w.contains(')') {
            push_trailing(&mut out, w);
            i += 1;
            continue;
        }
        if !keep_refs && is_reference(w) {
            // A group `(#NN by Name)` runs to its closing parenthesis.
            let mut next = i + 1;
            if w.starts_with('(') && !w.contains(')') {
                while next < words.len() && !words[next].contains(')') {
                    next += 1;
                }
                next += 1;
            }
            let pr = out.last().is_some_and(|l| {
                let l = l.trim_start_matches('(').to_ascii_lowercase();
                ["pr", "issue", "pull"].contains(&l.as_str())
            });
            if pr {
                out.pop();
                if out.last().is_some_and(|l| l.eq_ignore_ascii_case("see")) {
                    out.pop();
                }
            }
            push_trailing(&mut out, words[next.saturating_sub(1).min(words.len() - 1)]);
            i = next.min(words.len());
            // The `by Name` a reference is credited with, after it.
            if words.get(i).is_some_and(|n| n.eq_ignore_ascii_case("by"))
                && let Some(end) = name_run(&words[i + 1..])
            {
                i += 1 + end;
                continue;
            }
            // The `(Name)` of `See PR #NN (Name).`.
            if pr
                && let Some(n) = words.get(i)
                && n.starts_with('(')
                && !n.contains('#')
            {
                push_trailing(&mut out, n);
                i += 1;
            }
            continue;
        }
        out.push(w.to_string());
        i += 1;
    }
    out.join(" ")
}

/// The number of the words at the start of `words` that name a person after a
/// `by`: capitalised words, to the end of the bullet or a lowercase word. `None`
/// when the first word is not capitalised.
fn name_run(words: &[&str]) -> Option<usize> {
    let run = words
        .iter()
        .take_while(|w| w.chars().next().is_some_and(char::is_uppercase))
        .count();
    (run > 0).then_some(run)
}

/// Whether `word` is an issue or pull request number (`#86`, `(#86)`, `(#86).`).
fn is_reference(word: &str) -> bool {
    let core = word
        .trim_start_matches('(')
        .trim_end_matches(|c: char| ")., ;:".contains(c));
    core.strip_prefix('#')
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

/// Appends the sentence marks at the end of `word` to the last word of `out`,
/// unless that word has them already.
fn push_trailing(out: &mut [String], word: &str) {
    let marks: String = word
        .chars()
        .rev()
        .take_while(|c| ".,;:".contains(*c))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if let Some(last) = out.last_mut()
        && !marks.is_empty()
        && !last.ends_with(marks.chars().next().unwrap_or(' '))
    {
        last.push_str(&marks);
    }
}

/// One bullet's text without markdown links, emphasis and code ticks, and
/// without the "by @user in <url>" tail GitHub adds to generated notes. Not
/// cut: the callers cut it to their limit (a security line is never cut).
fn tidy(item: &str) -> String {
    // Cut first so that a huge bullet does not cost a pass over all of it.
    let item = head(item, MAX_SOURCE_CHARS);
    let mut s = strip_emoji(&strip_links(&item));
    for mark in ["**", "__", "`"] {
        s = s.replace(mark, "");
    }
    if let Some(i) = s.find(" by @") {
        let tail = &s[i + 5..];
        if tail
            .split_once(" in ")
            .is_some_and(|(_, u)| u.starts_with("http"))
        {
            s.truncate(i);
        }
    }
    s.split_whitespace()
        .filter(|w| !is_rule(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The recap lines for `bullets`: each a `* ` item, wrapped to the width of a
/// `.changes` line with continuation lines indented under the text. Every
/// line is kept; whether they fit [`MAX_LINES`] is decided by the caller (see
/// [`fits_lines`]).
pub(super) fn bullet_lines(bullets: &[String]) -> Vec<String> {
    bullets.iter().flat_map(|b| wrap(b)).collect()
}

/// Whether `bullets` wrap to at most [`MAX_LINES`] lines in all.
pub(super) fn fits_lines(bullets: &[String]) -> bool {
    bullets.iter().map(|b| wrap(b).len()).sum::<usize>() <= MAX_LINES
}

/// `text` as a `* ` item wrapped at [`WIDTH`] (the entry indents every line
/// by two more spaces). A word of dashes, equals signs or underscores is left
/// out: a line of only those, 20 or more, would end the `.changes` entry.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::from("* ");
    for word in text.split_whitespace().filter(|w| !is_rule(w)) {
        if current.len() + 1 + word.len() > WIDTH - 2 && current.trim() != "*" {
            lines.push(current);
            current = String::from("  ");
        }
        if !current.ends_with(' ') {
            current.push(' ');
        }
        current.push_str(word);
    }
    lines.push(current);
    lines
}

/// The "too many changes" form: a line saying so, then the newest release
/// pages, one URL each.
pub(super) fn link_lines(releases: &[RepoRelease]) -> Vec<String> {
    let mut lines = vec!["* Too many changes to list, see:".to_string()];
    lines.extend(
        releases
            .iter()
            .take(MAX_LINKS)
            .map(|r| format!("  {}", r.url)),
    );
    lines
}

/// The line for a changelog file too long to list and not summarized.
pub(super) fn file_line(file: &str) -> Vec<String> {
    vec![format!(
        "* Too many changes to list, see {file} in the sources"
    )]
}
