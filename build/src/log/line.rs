//! Line normalisation.
//!
//! Everything else in this module depends on getting this right, and getting it
//! wrong fails *silently*: `osc` prefixes every build line with an elapsed-time
//! field, so any anchor written against raw `rpmbuild` output — anything using
//! `^` — matches nothing at all. Verified on the run-1 log, where the obvious
//! `^\s+File "` traceback pattern found zero of the frames that were plainly
//! there.

use std::sync::LazyLock;

use regex::Regex;

/// `[    7s] ` — the only shape observed across the run-1 logs.
///
/// Deliberately narrow. A looser pattern such as `^\[[^]]*\]` would eat genuine
/// log content: Python list reprs, `[ERROR]` tags and compiler `[-Wflag]`
/// notes all start with a bracket. Requiring digits-then-`s` cannot collide
/// with those. Widen it only against a log that actually needs it.
static TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\s*\d+s\] ?").expect("static pattern"));

/// Strip the `osc` elapsed-time prefix, if present.
///
/// Lines emitted by `osc` itself (before the build starts) carry no prefix, so
/// absence is normal and not an error.
pub fn strip_timestamp(line: &str) -> &str {
    match TIMESTAMP.find(line) {
        Some(m) => &line[m.end()..],
        None => line,
    }
}

/// Whether the log is timestamp-prefixed at all.
///
/// A `false` here on a real `osc` log means the format changed and every anchor
/// in this module is about to under-match; the caller should say so loudly
/// rather than quietly return an empty extract.
pub fn has_timestamps(log: &str) -> bool {
    log.lines().take(200).any(|l| TIMESTAMP.is_match(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_observed_shapes() {
        // Both widths seen in the run-1 logs.
        assert_eq!(strip_timestamp("[    7s] hello"), "hello");
        assert_eq!(strip_timestamp("[   12s] hello"), "hello");
        assert_eq!(strip_timestamp("[ 1234s] hello"), "hello");
        assert_eq!(strip_timestamp("[0s] hello"), "hello");
    }

    #[test]
    fn preserves_traceback_indentation() {
        // The frame parser keys on leading whitespace, so the prefix must go
        // and the indentation must stay.
        let line = r#"[    7s]   File "/x/y.py", line 62, in get_args"#;
        assert_eq!(
            strip_timestamp(line),
            r#"  File "/x/y.py", line 62, in get_args"#
        );
    }

    #[test]
    fn leaves_unprefixed_lines_alone() {
        // osc's own pre-build chatter.
        assert_eq!(
            strip_timestamp("Building python-glances.spec for standard/x86_64"),
            "Building python-glances.spec for standard/x86_64"
        );
    }

    #[test]
    fn does_not_eat_bracketed_log_content() {
        // The reason the pattern is narrow.
        for line in [
            "['a', 'b']",
            "[ERROR] something",
            "[-Wunused-variable] warning",
            "[1s more text] not a stamp",
        ] {
            assert_eq!(strip_timestamp(line), line, "should not strip: {line}");
        }
    }

    #[test]
    fn detects_presence() {
        assert!(has_timestamps("[    7s] a\n[    8s] b"));
        assert!(!has_timestamps("plain\nlines\nonly"));
    }
}
