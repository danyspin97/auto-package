//! The one place versions are compared.
//!
//! Everything that asks "is this newer", "is this a pre-release" or "which of
//! these is the latest" goes through here, so the update sweep, the upstream
//! lookups, the PyPI client and the distro-bound checks cannot disagree.
//!
//! Two schemes:
//!
//! - [`Scheme::Pep440`]: the order PyPI uses. When both versions are PEP 440
//!   versions, that order decides (`1.0a1 < 1.0`, `1.2.3a < 1.2.3`,
//!   `1.0.0 == 1.0`, `1.0.post1 > 1.0`). A version that is not PEP 440 (an RPM
//!   `1.0~rc1`) falls back to the generic order, so a PyPI package packaged
//!   with an RPM-style version is still comparable.
//! - [`Scheme::Generic`]: what most other projects and RPM itself do. Versions
//!   split into runs of digits and runs of letters; digit runs compare as
//!   numbers (`1.10 > 1.9`, `1.01 == 1.1`), letter runs lexically and without
//!   case. A longer version is newer (`1.2.3.4 > 1.2.3`, `1.2.3a > 1.2.3`)
//!   unless what it adds marks a pre-release (`1.0rc1 < 1.0`, `1.0b2 < 1.0`) or
//!   follows a `~` (`1.0~rc1 < 1.0`); repository snapshots sort after the
//!   release (`1.0+git2025 > 1.0`, `1.0~git2025 < 1.0`). Trailing zero
//!   components do not make a version newer (`1.0.0 == 1.0`).
//!
//! Both schemes read an epoch (`2:1.0`, decided before anything else, a
//! missing one is 0) and ignore a leading `v`, `r` or `release-`. Empty input,
//! a word, an unexpanded macro (`%{version}`) or anything with characters a
//! version is not written with compares as `None`: "cannot tell" is never
//! "equal" or "not newer".

use std::cmp::Ordering;
use std::str::FromStr;

/// How two versions are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// PyPI's order, with the generic one for what is not PEP 440.
    Pep440,
    /// RPM-like order for everything else.
    Generic,
}

/// A version as a PEP 440 version, or `None` when it is not one.
pub fn parse_pep440(text: &str) -> Option<pep440_rs::Version> {
    pep440_rs::Version::from_str(text.trim()).ok()
}

/// `(epoch, rest)` of `2:1.0`; no epoch is 0.
pub fn split_epoch(v: &str) -> (u64, &str) {
    match v.split_once(':') {
        Some((e, rest)) if !rest.is_empty() => e.parse().map_or((0, v), |e| (e, rest)),
        _ => (0, v),
    }
}

/// A leading `v`, `V`, `r`, `R` or `release-` in front of a number (`v1.2`,
/// `r62`, `release-1.2`) is no part of the version.
pub fn strip_prefix(v: &str) -> &str {
    let digit = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    if let Some(head) = v.get(..8)
        && head.eq_ignore_ascii_case("release-")
        && digit(&v[8..])
    {
        return &v[8..];
    }
    match v.strip_prefix(['v', 'V', 'r', 'R']) {
        Some(rest) if digit(rest) => rest,
        _ => v,
    }
}

/// Starts like a release number and holds only the characters one is written
/// with.
fn is_plain(v: &str) -> bool {
    v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '~'))
}

/// Order two versions, `None` when either is not a version.
pub fn cmp(a: &str, b: &str, scheme: Scheme) -> Option<Ordering> {
    let (ea, a) = split_epoch(a.trim());
    let (eb, b) = split_epoch(b.trim());
    let (a, b) = (strip_prefix(a), strip_prefix(b));
    if scheme == Scheme::Pep440
        && let (Some(x), Some(y)) = (parse_pep440(a), parse_pep440(b))
    {
        return Some(ea.cmp(&eb).then_with(|| x.cmp(&y)));
    }
    if !is_plain(a) || !is_plain(b) {
        return None;
    }
    Some(ea.cmp(&eb).then_with(|| cmp_tokens(a, b)))
}

/// A total order for sorting candidates: [`cmp`] in the generic scheme, with
/// what is not a version compared by its pieces too (so sorting never panics
/// or goes inconsistent on a stray tag).
pub fn sort_cmp(a: &str, b: &str) -> Ordering {
    cmp(a, b, Scheme::Generic).unwrap_or_else(|| {
        let (ea, a) = split_epoch(a.trim());
        let (eb, b) = split_epoch(b.trim());
        ea.cmp(&eb)
            .then_with(|| cmp_tokens(strip_prefix(a), strip_prefix(b)))
    })
}

/// One piece of a version: a run of digits or of letters.
#[derive(Debug, Clone)]
struct Tok {
    num: bool,
    text: String,
    /// Sorts before the version without it: a `~` came right before it, or it
    /// is a pre-release word.
    before: bool,
}

const VCS_WORDS: &[&str] = &["git", "svn", "cvs", "hg", "bzr", "darcs"];
const PRERELEASE_WORDS: &[&str] = &[
    "rc",
    "alpha",
    "beta",
    "pre",
    "prerelease",
    "preview",
    "dev",
    "snapshot",
    "nightly",
];

fn is_prerelease_word(w: &str) -> bool {
    PRERELEASE_WORDS.contains(&w) || VCS_WORDS.contains(&w)
}

fn tokens(v: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    let mut tilde = false;
    let mut chars = v.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() {
            let digit = c.is_ascii_digit();
            let mut run = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() && d.is_ascii_digit() == digit {
                    run.push(d.to_ascii_lowercase());
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok {
                num: digit,
                text: run,
                before: tilde,
            });
            tilde = false;
        } else {
            tilde |= c == '~';
            chars.next();
        }
    }
    // Words that order before the plain release: not the repository snapshots
    // (`+git2025` is after it; a `~` in front already marked those), and `a`/`b`
    // only as a PEP 440 style `1.0a1`, not as the `1.2.3a` of a letter release.
    for i in 0..out.len() {
        if out[i].num {
            continue;
        }
        let w = out[i].text.as_str();
        let pep_letter = matches!(w, "a" | "b")
            && i > 0
            && out[i - 1].num
            && out.get(i + 1).is_some_and(|t| t.num);
        let word =
            (PRERELEASE_WORDS.contains(&w) && w != "snapshot" && w != "nightly") || pep_letter;
        if word {
            out[i].before = true;
        }
    }
    out
}

fn cmp_tokens(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (tokens(a), tokens(b));
    for (x, y) in ta.iter().zip(&tb) {
        let ord = match (x.num, y.num) {
            (true, true) => {
                let (p, q) = (
                    x.text.trim_start_matches('0'),
                    y.text.trim_start_matches('0'),
                );
                p.len().cmp(&q.len()).then_with(|| p.cmp(q))
            }
            (false, false) => x.text.cmp(&y.text),
            (true, false) => {
                if y.before {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if x.before {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    let (longer, flip) = match ta.len().cmp(&tb.len()) {
        Ordering::Equal => return Ordering::Equal,
        Ordering::Greater => (&ta[tb.len()..], false),
        Ordering::Less => (&tb[ta.len()..], true),
    };
    // Trailing zeros add nothing: `1.0.0 == 1.0`.
    let rest = longer
        .iter()
        .find(|t| !(t.num && !t.before && t.text.chars().all(|c| c == '0')));
    let ord = match rest {
        None => Ordering::Equal,
        Some(t) if t.before => Ordering::Less,
        Some(_) => Ordering::Greater,
    };
    if flip { ord.reverse() } else { ord }
}

/// Whether a version carries a pre-release marker as a whole word (`rc`,
/// `alpha`, `beta`, `pre`, `dev`, `snapshot`, `git`...), a `~`, or a PEP 440
/// `1.0a1`/`2.0b3`: `1.0rc1` and `2.0-beta2` are, `1.0.src`, `3.2.1`,
/// `1.2.3a` and `1.0-prefix` are not.
pub fn is_prerelease(version: &str) -> bool {
    let toks = tokens(version);
    version.contains('~')
        || toks
            .iter()
            .any(|t| !t.num && (is_prerelease_word(&t.text) || t.before))
}

/// Whether a version names a commit or a nightly build of a repository rather
/// than a release: `1.2+git20260727.abc123`, `3.0~svn4411`, `1.0.snapshot`. A
/// VCS word counts only when a number follows it.
pub fn is_snapshot(version: &str) -> bool {
    let toks = tokens(version);
    toks.iter().enumerate().any(|(i, t)| {
        !t.num
            && (matches!(t.text.as_str(), "snapshot" | "nightly")
                || (VCS_WORDS.contains(&t.text.as_str()) && toks.get(i + 1).is_some_and(|n| n.num)))
    })
}

/// The leading dotted numbers of a version: `5.3p20` is `[5, 3]`, `v1.2.3.4`
/// is `[1, 2, 3, 4]`. An epoch is not one of them.
pub fn leading_numbers(v: &str) -> Vec<u64> {
    let v = strip_prefix(split_epoch(v.trim()).1);
    let end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    v[..end]
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

/// Order the leading numbers both versions have (`5.3p20` against `5.4` looks
/// at `[5, 3]` and `[5, 4]`), `None` when either has none.
pub fn cmp_common_prefix(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (leading_numbers(a), leading_numbers(b));
    let n = a.len().min(b.len());
    (n > 0).then(|| a[..n].cmp(&b[..n]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::{Equal, Greater, Less};
    use Scheme::{Generic, Pep440};

    /// (a, b, a compared with b) in the generic scheme.
    const GENERIC: &[(&str, &str, Ordering)] = &[
        ("1.10", "1.9", Greater),
        ("1.9", "1.10", Less),
        ("13.2.0", "13.0.2", Greater),
        ("13.0.2", "13.2.0", Less),
        ("1.2.3.4", "1.2.3", Greater),
        ("1.2.3.4.5", "1.2.3.4", Greater),
        ("1.2.3.4", "1.2.3.5", Less),
        ("1.2.3a", "1.2.3", Greater),
        ("1.2.3a", "1.10", Less),
        ("1.0rc1", "1.0", Less),
        ("1.0-rc1", "1.0", Less),
        ("1.0RC1", "1.0rc1", Equal),
        ("1.0rc1", "1.0rc2", Less),
        ("1.0b2", "1.0", Less),
        ("1.0a1", "1.0b1", Less),
        ("1.0-beta2", "1.0-beta10", Less),
        ("1.0~rc1", "1.0", Less),
        ("1.0~git20260101", "1.0", Less),
        ("1.0+git20260101", "1.0", Greater),
        ("1.0.1", "1.0rc2", Greater),
        ("1.0.0rc1", "1.0", Less),
        ("007", "7", Equal),
        ("1.01", "1.1", Equal),
        ("1.0", "1.0", Equal),
        ("1.0.0", "1.0", Equal),
        ("1.0", "1.0.0.0", Equal),
        ("1.0.0.1", "1.0", Greater),
        ("2:1.0", "1:9.9", Greater),
        ("2:1.0", "1.0", Greater),
        ("0:1.0", "1.0", Equal),
        ("1:0.1", "9.9", Greater),
        ("v1.2", "1.2", Equal),
        ("V1.2", "1.2", Equal),
        ("r62", "r63", Less),
        ("r62", "61", Greater),
        ("release-1.2", "1.2", Equal),
        ("release-1.10", "release-1.9", Greater),
        ("1.0", "1.0.post1", Less),
        ("20260101", "20251231", Greater),
        ("5.3p20", "5.3", Greater),
    ];

    #[test]
    fn generic_order() {
        for &(a, b, want) in GENERIC {
            assert_eq!(cmp(a, b, Generic), Some(want), "{a} vs {b}");
            assert_eq!(cmp(b, a, Generic), Some(want.reverse()), "{b} vs {a}");
            assert_eq!(sort_cmp(a, b), want, "sort {a} vs {b}");
        }
    }

    const PEP440: &[(&str, &str, Ordering)] = &[
        ("1.10.0", "1.9.0", Greater),
        ("1.0.0", "1.0", Equal),
        ("2.0.0rc1", "2.0.0", Less),
        ("1.2.3a", "1.2.3", Less),
        ("1.0a1", "1.0", Less),
        ("1.0.post1", "1.0", Greater),
        ("1.0.dev1", "1.0a1", Less),
        ("1.0+git2025", "1.0", Greater),
        ("v1.2", "1.2", Equal),
        ("2:1.0", "1:9.9", Greater),
        ("1!1.0", "9.9", Greater),
        ("0.1.14.post1", "0.1.11", Greater),
        // Not PEP 440: the generic order decides.
        ("1.0~rc1", "1.0", Less),
        ("1.0", "1.0~rc1", Greater),
        ("r62", "r63", Less),
    ];

    #[test]
    fn pep440_order() {
        for &(a, b, want) in PEP440 {
            assert_eq!(cmp(a, b, Pep440), Some(want), "{a} vs {b}");
            assert_eq!(cmp(b, a, Pep440), Some(want.reverse()), "{b} vs {a}");
        }
    }

    #[test]
    fn what_is_not_a_version_does_not_compare() {
        for junk in [
            "",
            " ",
            "latest",
            "%{version}",
            "%{ver}",
            "1.0%{?x}",
            "abc",
            "1.0 beta",
            "not-a-version",
            "v",
            "2:",
            ":1.0",
            "x:1.0",
        ] {
            for scheme in [Generic, Pep440] {
                assert_eq!(cmp(junk, "1.0", scheme), None, "{junk:?}");
                assert_eq!(cmp("1.0", junk, scheme), None, "{junk:?}");
            }
        }
    }

    #[test]
    fn sort_cmp_is_total() {
        assert_eq!(sort_cmp("1.0", "latest"), sort_cmp("1.0", "latest"));
        assert_eq!(sort_cmp("1.10", "1.9"), Greater);
        assert_eq!(sort_cmp("x", "x"), Equal);
    }

    #[test]
    fn prefixes_come_off_only_in_front_of_a_number() {
        assert_eq!(strip_prefix("v1.2"), "1.2");
        assert_eq!(strip_prefix("V1.2"), "1.2");
        assert_eq!(strip_prefix("r62"), "62");
        assert_eq!(strip_prefix("release-1.2"), "1.2");
        assert_eq!(strip_prefix("Release-1.2"), "1.2");
        assert_eq!(strip_prefix("release-x"), "release-x");
        assert_eq!(strip_prefix("rc1"), "rc1");
        assert_eq!(strip_prefix("version"), "version");
        assert_eq!(strip_prefix("é"), "é");
        assert_eq!(strip_prefix(""), "");
    }

    #[test]
    fn prerelease_words_are_whole_words() {
        for v in [
            "1.0rc1",
            "2.0-beta2",
            "1.3.12-pre3",
            "1.0~git",
            "1.0~1",
            "3.0.dev1",
            "1.0+git20260101",
            "1.0a1",
            "2.0b3",
            "1.0.0-alpha",
            "1.0-snapshot",
        ] {
            assert!(is_prerelease(v), "{v}");
        }
        for v in [
            "1.2.3",
            "4.3.8.src",
            "1.2.3a",
            "2.1.13-stable",
            "1.0-prefix",
            "1.0.arch",
            "1.0.post1",
            "",
        ] {
            assert!(!is_prerelease(v), "{v}");
        }
    }

    #[test]
    fn snapshots_name_a_commit() {
        for v in [
            "1.2+git20260727.abc123",
            "3.0~svn4411",
            "1.0~git.20251221",
            "2+git20260101",
            "1.0+hg123",
            "1.0.git20260101",
            "1.0+bzr9",
            "1.0+darcs20260101",
            "1.0.snapshot",
        ] {
            assert!(is_snapshot(v), "{v}");
        }
        for v in ["1.2.3", "1.0rc1", "1.0+gitabc", "1.0-digit", "2.0", ""] {
            assert!(!is_snapshot(v), "{v}");
        }
    }

    #[test]
    fn leading_numbers_stop_at_the_first_letter() {
        assert_eq!(leading_numbers("r62"), vec![62]);
        assert_eq!(leading_numbers("v1.2.3.4"), vec![1, 2, 3, 4]);
        assert_eq!(leading_numbers("1.2.3a"), vec![1, 2, 3]);
        assert_eq!(leading_numbers("5.3p20"), vec![5, 3]);
        assert_eq!(leading_numbers("2:1.5"), vec![1, 5]);
        assert_eq!(leading_numbers("release-3.4"), vec![3, 4]);
        assert_eq!(leading_numbers("1..2"), vec![1]);
        assert!(leading_numbers("abc").is_empty());
        assert!(leading_numbers("").is_empty());
    }

    #[test]
    fn common_prefix_compares_what_both_have() {
        assert_eq!(cmp_common_prefix("5.4", "5.3"), Some(Greater));
        assert_eq!(cmp_common_prefix("5.3p20", "5.3"), Some(Equal));
        assert_eq!(cmp_common_prefix("6.0p1", "5.3"), Some(Greater));
        assert_eq!(cmp_common_prefix("5", "5.3"), Some(Equal));
        assert_eq!(cmp_common_prefix("4", "5.3"), Some(Less));
        assert_eq!(cmp_common_prefix("abc", "5.3"), None);
    }

    #[test]
    fn pep440_parse_trims() {
        assert!(parse_pep440(" 2.0.0rc1 ").is_some());
        assert!(parse_pep440("not a version").is_none());
    }
}
