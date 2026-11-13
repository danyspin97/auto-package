//! Turning the notes of releases into the lines of a `.changes` recap: the
//! bullets of the notes, cleaned, or the link-only form when they are too many.

use super::markdown::{is_rule, strip_comments, strip_emoji, strip_links};
use super::routine::is_routine;
use crate::sources::RepoRelease;

/// The most bullets a recap lists as they are; more are summarized.
pub(super) const MAX_BULLETS: usize = 10;
/// The most links the "too many changes" form lists.
const MAX_LINKS: usize = 5;
/// Width of a `.changes` line, including its two-space indent.
const WIDTH: usize = 67;
/// A single-release bullet is cut to this many characters: release bodies are
/// untrusted.
const MAX_BULLET_CHARS: usize = 200;
/// A bullet's source is read up to this many characters.
const MAX_SOURCE_CHARS: usize = 20_000;
/// The most lines a recap takes.
const MAX_LINES: usize = 40;

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
            return Notes::Bullets(
                bullets
                    .iter()
                    .map(|b| plain_cut(b, MAX_BULLET_CHARS))
                    .collect(),
            );
        }
    }
    let text: String = with_notes
        .iter()
        .map(|r| format!("## {}\n{}\n", r.tag, strip_comments(&r.body).trim()))
        .collect();
    Notes::Long(text.chars().take(20_000).collect())
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
        .map(|item| tidy(item))
        .filter(|item| item.chars().any(char::is_alphanumeric))
        .collect()
}

/// One bullet's text without markdown links, emphasis and code ticks, and
/// without the "by @user in <url>" tail GitHub adds to generated notes. Not
/// cut: the callers cut it to their limit (a security line is never cut).
fn tidy(item: &str) -> String {
    // Cut first so that a huge bullet does not cost a pass over all of it.
    let item: String = item.chars().take(MAX_SOURCE_CHARS).collect();
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

/// `text` cut to `max` characters, the `...` included, as the single-release
/// form has always cut its bullets.
fn plain_cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 3).collect();
    format!("{}...", cut.trim_end())
}

/// The recap lines for `bullets`: each a `* ` item, wrapped to the width of a
/// `.changes` line with continuation lines indented under the text, at most
/// [`MAX_LINES`] of them.
pub(super) fn bullet_lines(bullets: &[String]) -> Vec<String> {
    bullets
        .iter()
        .flat_map(|b| wrap(b))
        .take(MAX_LINES)
        .collect()
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
