//! The limits of a recap over several releases, and what cuts a bullet to
//! them. Every number of those limits is in this file, and the prompt of the
//! model and the tests read them from here.

/// Most bullets one release lists.
pub(super) const PER_RELEASE: usize = 12;
/// Most bullets a recap over several releases lists in all.
pub(super) const TOTAL: usize = 40;
/// Most characters of a bullet that is cut: a longer bullet is cut at a
/// sentence or clause end within this many characters (see [`cut`]).
pub(super) const BULLET_CHARS: usize = 160;
/// Most characters of a bullet with no sentence or clause end within
/// [`BULLET_CHARS`]: it is kept whole up to this many, and above it is cut at
/// the last word boundary before this many (see [`cut`]).
pub(super) const BULLET_CEILING: usize = 400;
/// The bullets an older release is shrunk to, oldest releases first, when the
/// recap is over the limits. After that each one is a single `see` line.
pub(super) const SHRUNK: usize = 6;

/// Markers of a security fix: a CVE id, a SUSE or openSUSE bug number, or a
/// GitHub issue. Their bullets are never cut, never dropped.
const SECURITY: &[&str] = &["cve-", "bsc#", "boo#", "gh#"];
/// Words of a change that the user has to act on or that changes what works:
/// listed first in their release.
const BREAKING: &[&str] = &[
    "break",
    "remov",
    "deprecat",
    "no longer",
    "rename",
    "require",
    "incompatible",
    "minimum",
];
/// The words of [`BREAKING`] that must be the whole word: `drop` is also a
/// part of `backdrop` and `raindrops`.
const BREAKING_WORDS: &[&str] = &["drop", "drops", "dropped", "dropping"];

/// Whether `line` names a security fix (see [`SECURITY`]).
pub(super) fn is_security(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SECURITY.iter().any(|m| lower.contains(m))
}

/// Whether `line` is a breaking change: marked `Breaking:`, or worded like one
/// (see [`BREAKING`]).
pub(super) fn is_breaking(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("breaking:")
        || BREAKING.iter().any(|w| has_word(&lower, w, false))
        || BREAKING_WORDS.iter().any(|w| has_word(&lower, w, true))
}

/// Whether `text` holds `word` where a word starts: at the start of the text or
/// after a character that is not a letter, a digit, `-` or `_`. With `whole`
/// the word must also end there (before such a character or at the end).
fn has_word(text: &str, word: &str, whole: bool) -> bool {
    let joins = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let starts = text[..at].chars().next_back().is_none_or(|c| !joins(c));
        let ends = !whole
            || text[at + word.len()..]
                .chars()
                .next()
                .is_none_or(|c| !joins(c));
        starts && ends
    })
}

/// The order of a release's bullets: breaking changes, then security fixes,
/// then the rest. A sort by this key is stable, so the rest keep their order.
pub(super) fn rank(line: &str) -> u8 {
    if is_breaking(line) {
        0
    } else if is_security(line) {
        1
    } else {
        2
    }
}

/// [`cut`] for a bullet of a recap, except that a security line is never cut:
/// its reference is the fact the reader needs.
pub(super) fn cut_bullet(text: &str, limit: usize) -> String {
    if is_security(text) {
        text.trim().to_string()
    } else {
        cut(text, limit)
    }
}

/// The first `max` characters of `text`, cut back to its last whitespace when
/// there is one, so that a word is not cut in two. A text with no whitespace
/// in its first `max` characters is cut at `max`.
pub(super) fn head(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    match head.rfind(char::is_whitespace) {
        Some(at) if at > 0 => head[..at].to_string(),
        _ => head,
    }
}

/// `text` as a bullet. A text of at most `limit` characters is kept. A longer
/// one is cut at its last sentence end within `limit` characters, else at its
/// last clause end (`, ` or `; `) within it, with no `...` added. A text with
/// neither is kept whole up to [`BULLET_CEILING`] characters; a longer one is
/// cut at the last word boundary before that. A cut never ends on a dangling
/// `,`, `;`, `:` or a joining word (see [`undangle`]).
pub(super) fn cut(text: &str, limit: usize) -> String {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return undangle(text);
    }
    let kept = match boundary_within(&chars, limit) {
        Some(n) => n,
        None if chars.len() <= BULLET_CEILING => chars.len(),
        None => word_end_within(&chars, BULLET_CEILING),
    };
    let out = undangle(&chars[..kept].iter().collect::<String>());
    if out.is_empty() { undangle(text) } else { out }
}

/// The number of characters of the longest prefix of `chars` that is at most
/// `max` characters and ends at a word boundary: `max` itself when a whitespace
/// follows it, else the last whitespace before it. A text with no whitespace
/// before `max` is cut at `max`.
fn word_end_within(chars: &[char], max: usize) -> usize {
    if chars.get(max).is_some_and(|c| c.is_whitespace()) {
        return max;
    }
    chars[..max]
        .iter()
        .rposition(|c| c.is_whitespace())
        .filter(|&at| at > 0)
        .unwrap_or(max)
}

/// The number of characters a cut keeps: up to the last sentence end within the
/// first `limit` characters, else up to the last clause end there (the `,` or
/// `;` itself is not kept). `None` when the text has neither.
fn boundary_within(chars: &[char], limit: usize) -> Option<usize> {
    let within = limit.min(chars.len());
    let sentence = (0..within).rev().find(|&i| {
        chars[i] == '.' && chars.get(i + 1).is_none_or(|c| c.is_whitespace()) && {
            let word = word_before(chars, i);
            word.chars().count() >= 2 && !word.contains('.')
        }
    });
    if let Some(end) = sentence {
        return Some(end + 1);
    }
    (0..within).rev().find(|&i| {
        matches!(chars[i], ',' | ';') && chars.get(i + 1).is_some_and(|c| c.is_whitespace()) && {
            let word = word_before(chars, i);
            word.chars().count() >= 2
                && !word.contains('.')
                && word.chars().any(char::is_alphabetic)
        }
    })
}

/// The word that ends just before `chars[at]`, or the empty string when a
/// whitespace or the start comes first.
fn word_before(chars: &[char], at: usize) -> String {
    chars[..at]
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .collect()
}

/// `text` without a dangling end: trailing `,`, `;` or `:` and a last word
/// that joins to what would follow (`and`, `or`, `but`) are dropped, so that
/// the bullet reads as a whole sentence or phrase.
pub(super) fn undangle(text: &str) -> String {
    let mut s = text.trim_end().to_string();
    loop {
        let before = s.len();
        while s.ends_with([',', ';', ':']) {
            s.pop();
            s = s.trim_end().to_string();
        }
        if let Some(at) = s.rfind(char::is_whitespace) {
            let last = &s[at + 1..];
            if ["and", "or", "but"]
                .iter()
                .any(|w| last.eq_ignore_ascii_case(w))
            {
                s.truncate(at);
                s = s.trim_end().to_string();
            }
        }
        if s.len() == before {
            return s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bullet_under_the_limit_is_untouched() {
        assert_eq!(cut("alpha beta", BULLET_CHARS), "alpha beta");
        let exact = "x".repeat(BULLET_CHARS);
        assert_eq!(cut(&exact, BULLET_CHARS), exact);
        let under = "word ".repeat(31);
        assert_eq!(cut(&under, BULLET_CHARS), under.trim());
    }

    #[test]
    fn a_long_single_sentence_bullet_is_kept_whole() {
        let text = "word ".repeat(60);
        let text = text.trim();
        assert!(text.chars().count() > BULLET_CHARS);
        assert_eq!(cut(text, BULLET_CHARS), text);
    }

    #[test]
    fn a_long_bullet_is_cut_at_a_clause_end_without_an_ellipsis() {
        let text = format!("Add a new option to the parser, {}", "word ".repeat(40));
        let got = cut(text.trim(), BULLET_CHARS);
        assert_eq!(got, "Add a new option to the parser");
        assert!(!got.contains("..."), "{got}");
    }

    #[test]
    fn a_bullet_of_three_hundred_characters_with_no_boundary_is_kept_whole() {
        let text = "word ".repeat(61);
        let text = text.trim();
        assert_eq!(text.chars().count(), 304);
        assert_eq!(cut(text, BULLET_CHARS), text);
    }

    #[test]
    fn a_bullet_over_the_ceiling_with_no_boundary_is_cut_at_a_word() {
        let text = "word ".repeat(120);
        let text = text.trim();
        let got = cut(text, BULLET_CHARS);
        assert!(got.chars().count() <= BULLET_CEILING, "{got}");
        assert!(got.ends_with("word"), "{got}");
        assert!(!got.contains("..."), "{got}");
        assert!(text.starts_with(&got));
    }

    #[test]
    fn e_g_is_not_a_sentence_end() {
        let text = format!("Use the flag e.g. for the new mode {}", "word ".repeat(30));
        let text = text.trim();
        assert!(text.chars().count() > BULLET_CHARS);
        assert_eq!(cut(text, BULLET_CHARS), text);
    }

    #[test]
    fn a_cut_at_a_sentence_end_that_fits_keeps_the_sentence() {
        let long = "First one. Second part is long enough to need cutting for the limit here";
        assert_eq!(cut(long, 60), "First one.");
        assert_eq!(cut(long, BULLET_CHARS), long);
    }

    #[test]
    fn a_cut_never_ends_on_a_dangling_mark_or_joining_word() {
        assert_eq!(cut("alpha, beta gamma", 11), "alpha");
        assert_eq!(cut("alpha and, beta gamma", 12), "alpha");
        let words = "word ".repeat(40);
        assert_eq!(
            cut(&format!("Add a thing and, {words}"), BULLET_CHARS),
            "Add a thing"
        );
        assert_eq!(
            cut(&format!("Add a thing, {words}"), BULLET_CHARS),
            "Add a thing"
        );
        assert_eq!(
            cut(&format!("Add a thing and {words}"), BULLET_CHARS),
            format!("Add a thing and {}", words.trim())
        );
        assert_eq!(cut("Add a thing,", 100), "Add a thing");
        assert_eq!(cut("Add a thing and", 100), "Add a thing");
        assert_eq!(cut("Keep x; then", 100), "Keep x; then");
        let text = "Preserve constructor signatures on deprecated classes, including in Sphinx autodoc, instead of displaying the warning wrapper's generic arguments.";
        let got = cut(text, 100);
        assert!(got.chars().count() <= 100, "{got}");
        assert!(!got.ends_with(','), "{got}");
        assert!(!got.contains("..."), "{got}");
    }

    #[test]
    fn a_version_list_is_not_cut_at_its_commas() {
        let text = format!("Support 1.2, 1.3 and 1.4 {}", "word ".repeat(40));
        let got = cut(text.trim(), BULLET_CHARS);
        assert_eq!(got, text.trim());
    }

    #[test]
    fn a_head_is_cut_at_its_last_whitespace_or_else_at_its_length() {
        assert_eq!(head("aaa bbb ccc", 6), "aaa");
        assert_eq!(head("abcdef ghi jkl", 10), "abcdef");
        assert_eq!(head("abcdef", 3), "abc");
        assert_eq!(head("short", 10), "short");
        assert_eq!(head("exact", 5), "exact");
        let none = "x".repeat(25_000);
        assert_eq!(head(&none, 20_000).chars().count(), 20_000);
        let words = "word ".repeat(5_000);
        let got = head(&words, 20_000);
        assert!(
            got.chars().count() <= 20_000 && got.ends_with("word"),
            "{got}"
        );
    }

    #[test]
    fn a_security_line_is_never_cut() {
        let line = format!(
            "Fix CVE-2024-1234 in the parser {}",
            "and more words ".repeat(20)
        );
        assert_eq!(cut_bullet(&line, 200), line.trim());
        assert_eq!(
            cut_bullet("Add a long thing and more words here", 12),
            "Add a long thing and more words here"
        );
    }

    #[test]
    fn security_lines_and_breaking_lines_are_recognised() {
        assert!(is_security("Fix CVE-2024-1234 in the parser"));
        assert!(is_security("Fix a crash (bsc#1234)"));
        assert!(is_security("boo#99 update"));
        assert!(is_security("Fixes gh#org/repo#7"));
        assert!(!is_security("Fix #12 in the parser"));
        assert!(is_breaking("Breaking: the old alias is gone"));
        assert!(is_breaking("Remove the legacy session class"));
        assert!(!is_breaking("Add a cache"));
        assert!(!is_breaking("Fix the dropdown menu crash"));
        assert!(is_breaking("Drop support for Python 3.8"));
        assert!(is_breaking("Dropped the legacy session class"));
        assert!(is_breaking("Python 3.8 dropped."));
        assert!(is_breaking("Drop, then rebuild"));
        assert!(is_breaking("Stop after the drop"));
        assert!(!is_breaking("Fix the backdrop flicker"));
        assert!(!is_breaking("Keep the raindrops working"));
        assert!(!is_breaking("Fix the dropdown menu crash"));
        assert!(!is_breaking("Add a drop-in shim"));
        assert!(rank("Remove x") < rank("Fix CVE-1-2") && rank("Fix CVE-1-2") < rank("Add y"));
    }
}
