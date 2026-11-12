//! A failure kind as a reader says it: `InstallFailed` reads `install failed`.
//! Only the leading word of a text is changed, and only when it is a CamelCase
//! word of two or more parts, so free text that starts with a plain word is
//! left as it is.

/// `text` with its leading CamelCase word split into lower-case words:
/// `InstallFailed` becomes `install failed`, `SRPMLintFailed` becomes
/// `SRPM lint failed` and `OBSTimeout` becomes `OBS timeout`. A run of capitals
/// that is a word of its own stays capitals. The rest of the text is kept as
/// it is, so `RpmParseError at line 3: x` becomes `rpm parse error at line 3: x`.
pub fn humanise(text: &str) -> String {
    let end = text
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(text.len());
    let (word, rest) = text.split_at(end);
    let parts = camel_parts(word);
    if parts.len() < 2 {
        return text.to_string();
    }
    let words: Vec<String> = parts
        .iter()
        .map(|p| {
            if is_acronym(p) {
                p.to_string()
            } else {
                p.to_lowercase()
            }
        })
        .collect();
    format!("{}{rest}", words.join(" "))
}

/// `word` split before each capital that starts a word: after a lower-case
/// letter or digit, or before the last capital of a run that a lower-case
/// letter follows (`SRPMLint` gives `SRPM` and `Lint`).
fn camel_parts(word: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        let (at, c) = chars[i];
        if !c.is_uppercase() {
            continue;
        }
        let prev = chars[i - 1].1;
        let next_lower = chars.get(i + 1).is_some_and(|(_, n)| n.is_lowercase());
        if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
            parts.push(&word[start..at]);
            start = at;
        }
    }
    if start < word.len() {
        parts.push(&word[start..]);
    }
    parts
}

/// A word of two or more capitals and digits, such as `OBS` or `SRPM`.
fn is_acronym(word: &str) -> bool {
    word.chars().count() >= 2
        && word.chars().all(|c| c.is_uppercase() || c.is_ascii_digit())
        && word.chars().any(|c| c.is_uppercase())
}
