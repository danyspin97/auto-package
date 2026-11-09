//! Finding a package in a listing by the name the pipeline asks for.
//!
//! A listing spells each package the way OBS keeps it, which is not always the
//! spelling the pipeline derives from a PyPI name (`python-Authlib` for
//! `python-authlib`, `python-pydantic_core` for `python-pydantic-core`). The
//! lookup tries, in order: the exact spelling; the same name up to case; the
//! same project up to separators. Two names are the same project in the last
//! step when [`project_key`] equal, which is PyPI's own rule (PEP 503), so
//! `python-whois` and `pythonwhois` are two projects, not one.

/// The key two names share when they name the same PyPI project: lowercase,
/// with each run of `-`, `_` and `.` read as one `-` (PEP 503 normalization).
///
/// Unlike [`super::fuzzy_key`], separators are not dropped, so `python-whois`
/// (`python-whois`) and `pythonwhois` (`pythonwhois`) keep distinct keys.
pub fn project_key(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !key.ends_with('-') {
                key.push('-');
            }
        } else {
            key.extend(c.to_lowercase());
        }
    }
    key
}

/// What a lookup found among the listed names.
#[derive(Debug, PartialEq, Eq)]
pub enum Listed<'a> {
    /// The listing spells the project as requested, or as one spelling of it
    /// that differs only in case or separators.
    Found(&'a str),
    /// No listed name is the same project.
    Absent,
    /// Several listed names are the same project up to separators, and none
    /// is the requested name up to case. The caller must not pick one.
    /// Sorted, so the message does not depend on the listing order.
    Ambiguous(Vec<&'a str>),
}

/// Look `requested` up among `listed`, in the order described in the module
/// doc. Among spellings that differ only in case, the one equal to the
/// lowercase request wins, else the smallest, so the answer does not depend on
/// the listing order.
pub fn find_listed<'a>(requested: &str, listed: impl IntoIterator<Item = &'a str>) -> Listed<'a> {
    let key = project_key(requested);
    let mut same: Vec<&'a str> = listed
        .into_iter()
        .filter(|p| project_key(p) == key)
        .collect();
    same.sort_unstable();
    if let Some(exact) = same.iter().copied().find(|p| *p == requested) {
        return Listed::Found(exact);
    }
    let lower = requested.to_lowercase();
    let by_case: Vec<&'a str> = same
        .iter()
        .copied()
        .filter(|p| p.to_lowercase() == lower)
        .collect();
    if let Some(&first) = by_case.first() {
        let lowercase = by_case.iter().copied().find(|p| *p == lower);
        return Listed::Found(lowercase.unwrap_or(first));
    }
    match same.as_slice() {
        [] => Listed::Absent,
        [only] => Listed::Found(*only),
        [first, rest @ ..]
            if rest
                .iter()
                .all(|p| p.to_lowercase() == first.to_lowercase()) =>
        {
            Listed::Found(*first)
        }
        _ => Listed::Ambiguous(same),
    }
}

/// The message for a lookup that [`find_listed`] reported as ambiguous.
pub fn ambiguity_message(requested: &str, names: &[&str]) -> String {
    format!(
        "{requested} matches {} listed packages that are the same project up to \
         separators ({}); not choosing one, rename or remove the extra ones",
        names.len(),
        names.join(", ")
    )
}
