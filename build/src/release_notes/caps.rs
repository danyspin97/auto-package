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
    "drop",
    "deprecat",
    "no longer",
    "rename",
    "require",
    "incompatible",
    "minimum",
];

/// Whether `line` names a security fix (see [`SECURITY`]).
pub(super) fn is_security(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SECURITY.iter().any(|m| lower.contains(m))
}

/// Whether `line` is a breaking change: marked `Breaking:`, or worded like one
/// (see [`BREAKING`]).
pub(super) fn is_breaking(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("breaking:") || BREAKING.iter().any(|w| lower.contains(w))
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

/// `text` cut to `limit` characters, the `...` included, at the last word
/// boundary that fits; `text` as it is when it fits already.
pub(super) fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit.saturating_sub(3)).collect();
    let whole_word = text
        .chars()
        .nth(head.chars().count())
        .is_some_and(char::is_whitespace);
    let kept = if whole_word {
        head.as_str()
    } else {
        match head.rfind(char::is_whitespace) {
            Some(at) if at > 0 => &head[..at],
            _ => head.as_str(),
        }
    };
    format!("{}...", kept.trim_end())
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
    fn security_lines_and_breaking_lines_are_recognised() {
        assert!(is_security("Fix CVE-2024-1234 in the parser"));
        assert!(is_security("Fix a crash (bsc#1234)"));
        assert!(is_security("boo#99 update"));
        assert!(is_security("Fixes gh#org/repo#7"));
        assert!(!is_security("Fix #12 in the parser"));
        assert!(is_breaking("Breaking: the old alias is gone"));
        assert!(is_breaking("Remove the legacy session class"));
        assert!(!is_breaking("Add a cache"));
        assert!(rank("Remove x") < rank("Fix CVE-1-2") && rank("Fix CVE-1-2") < rank("Add y"));
    }
}
