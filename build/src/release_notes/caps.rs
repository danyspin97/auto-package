//! The limits of a recap over several releases, and what cuts a bullet to
//! them. Every number of those limits is in this file, and the prompt of the
//! model and the tests read them from here.

/// Most bullets one release lists.
pub(super) const PER_RELEASE: usize = 12;
/// Most bullets a recap over several releases lists in all.
pub(super) const TOTAL: usize = 40;
/// Most characters of a bullet, its `...` included, when it is cut.
pub(super) const BULLET_CHARS: usize = 100;
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

/// `text` as a bullet of at most `limit` characters, the `...` included. A
/// text that fits is kept. A longer one is cut at its last sentence end that
/// fits, else at the last word boundary that fits, with `...` after it. A cut
/// never ends on a dangling `,`, `;`, `:` or a joining word (see [`undangle`]).
pub(super) fn cut(text: &str, limit: usize) -> String {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return undangle(text);
    }
    if let Some(end) = sentence_end_within(&chars, limit) {
        return chars[..=end].iter().collect();
    }
    let head: String = chars[..limit.saturating_sub(3)].iter().collect();
    let whole_word = chars
        .get(limit.saturating_sub(3))
        .is_some_and(|c| c.is_whitespace());
    let kept = if whole_word {
        head.as_str()
    } else {
        match head.rfind(char::is_whitespace) {
            Some(at) if at > 0 => &head[..at],
            _ => head.as_str(),
        }
    };
    format!("{}...", undangle(kept))
}

/// The index of the last `.` within the first `limit` characters that ends a
/// sentence: a `.` followed by a space or by the end of the text, after a word
/// of two letters or more (`e.g.` and `v1.2` are not sentence ends).
fn sentence_end_within(chars: &[char], limit: usize) -> Option<usize> {
    (0..limit.min(chars.len())).rev().find(|&i| {
        chars[i] == '.' && chars.get(i + 1).is_none_or(|c| c.is_whitespace()) && {
            let word: String = chars[..i]
                .iter()
                .rev()
                .take_while(|c| !c.is_whitespace())
                .collect();
            word.chars().count() >= 2 && !word.contains('.')
        }
    })
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
    fn a_cut_is_at_a_word_boundary_and_within_the_limit() {
        assert_eq!(cut("alpha beta gamma", 12), "alpha...");
        assert_eq!(cut("alpha beta", 100), "alpha beta");
        assert_eq!(cut("abcdefghij", 6), "abc...");
        assert!(cut(&"word ".repeat(50), 100).chars().count() <= 100);
    }

    #[test]
    fn a_cut_at_a_sentence_end_that_fits_keeps_the_sentence() {
        let long = "First one. Second part is long enough to need cutting for the limit here";
        assert_eq!(cut(long, 60), "First one.");
        assert_eq!(
            cut("Use e.g. this option, then more words here", 20),
            "Use e.g. this..."
        );
    }

    #[test]
    fn a_cut_never_ends_on_a_dangling_mark_or_joining_word() {
        assert_eq!(cut("alpha, beta gamma", 11), "alpha...");
        assert_eq!(cut("alpha and beta gamma", 12), "alpha...");
        assert_eq!(cut("Add a thing,", 100), "Add a thing");
        assert_eq!(cut("Add a thing and", 100), "Add a thing");
        assert_eq!(cut("Keep x; then", 100), "Keep x; then");
        let text = "Preserve constructor signatures on deprecated classes, including in Sphinx autodoc, instead of displaying the warning wrapper's generic arguments.";
        let got = cut(text, 100);
        assert!(got.chars().count() <= 100, "{got}");
        assert!(got.ends_with("..."), "{got}");
        assert!(!got.contains(",..."), "{got}");
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
        assert!(cut_bullet("Add a long thing and more words here", 12).ends_with("..."));
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
