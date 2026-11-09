//! The identity of a failure reason for the repeat check. A reason often names
//! what changes between two attempts of the same failure: a line number, a
//! temporary directory, the commit message of an `osc ci`, a hash, a time. Two
//! reasons with the same key are the same failure.

use regex::Regex;
use std::sync::OnceLock;

/// Characters of a reason that decide whether two reasons are the same failure.
pub const KEY_CHARS: usize = 200;

/// The key of a failure reason: the reason with what changes between attempts
/// replaced, cut to [`KEY_CHARS`] characters.
pub fn failure_key(reason: &str) -> String {
    let mut text = reason.to_string();
    if let Ok(tmp) = std::env::var("TMPDIR") {
        if tmp.len() > 1 {
            if let Ok(re) = Regex::new(&format!(
                r"{}/\S*",
                regex::escape(tmp.trim_end_matches('/'))
            )) {
                text = re.replace_all(&text, "<tmp>").into_owned();
            }
        }
    }
    for (re, to) in rules() {
        text = re.replace_all(&text, *to).into_owned();
    }
    text.chars().take(KEY_CHARS).collect()
}

/// Whether two failure reasons are the same failure (see [`failure_key`]).
pub fn same_failure(a: &str, b: &str) -> bool {
    failure_key(a) == failure_key(b)
}

/// The replacements of a key, in order. The commit message of `osc ci -m` goes
/// first, so what it quotes does not matter; then the times and dates, the
/// temporary paths, the line numbers after a `:`, and the hashes.
fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r#"-m "[^"]*""#, r#"-m "<msg>""#),
            (r"\b\d{1,2}:\d{2}:\d{2}\b", "<time>"),
            (r"\b\d{4}-\d{2}-\d{2}\b", "<date>"),
            (r"/tmp/\S*", "<tmp>"),
            (r"\.tmp[A-Za-z0-9]{4,}", "<tmp>"),
            (r":\d+\b", ":N"),
            (r"\b[0-9a-f]{7,40}\b", "<hex>"),
        ]
        .into_iter()
        .map(|(pattern, to)| (Regex::new(pattern).expect("a valid pattern"), to))
        .collect()
    })
}
