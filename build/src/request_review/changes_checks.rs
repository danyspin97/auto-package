//! The newest `.changes` entry of the request: that it exists, that its
//! header and date are right and that its bullets say what the request does
//! (convention 40).

use regex::Regex;

use super::{Finding, Severity, ids};
use crate::risk_flags::RiskFlag;

/// What the `.changes` checks read.
pub struct ChangesCtx<'a> {
    pub package: &'a str,
    /// The `.changes` file name, for the findings.
    pub file: &'a str,
    /// The target's text, `None` for a package it does not have.
    pub base: Option<&'a str>,
    pub new: Option<&'a str>,
    pub base_version: Option<&'a str>,
    pub new_version: &'a str,
    /// The spec or a patch changed, was added or removed.
    pub packaging_changed: bool,
    pub patches_added: &'a [String],
    pub patches_removed: &'a [String],
    /// The request's `created` time, `2026-11-03T11:14:44`.
    pub created: &'a str,
}

/// A bullet of an entry: its first line, how many sub-bullets and links it
/// holds, and the line (1-based) it starts at in the file.
struct Bullet {
    text: String,
    items: usize,
    link: bool,
    line: usize,
}

struct Entry {
    separator: String,
    header: String,
    line: usize,
    bullets: Vec<Bullet>,
}

/// The text the new file adds above the base, and whether the base was kept
/// as the old entries.
fn added_text<'a>(base: Option<&str>, new: &'a str) -> (&'a str, bool) {
    let Some(base) = base else {
        return (new, true);
    };
    if let Some(prefix) = new.strip_suffix(base) {
        return (prefix, true);
    }
    // The old entries were edited: the new ones are what stands above the base's
    // newest entry when it is still there, else the first entry alone.
    let newest = base
        .split("\n-------------------------------------------------------------------\n")
        .next()
        .unwrap_or(base);
    let newest = newest.trim();
    if !newest.is_empty()
        && let Some(at) = new.find(newest)
    {
        return (&new[..at], false);
    }
    let second = new
        .match_indices("\n-------------------------------------------------------------------")
        .nth(0)
        .map(|(i, _)| i + 1);
    (&new[..second.unwrap_or(new.len())], false)
}

fn is_separator(l: &str) -> bool {
    crate::osc::is_changes_separator(l)
}

fn entries(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((at, line)) = lines.next() {
        if is_separator(line) {
            let header = lines
                .next_if(|(_, l)| !is_separator(l))
                .map_or("", |(_, l)| l);
            out.push(Entry {
                separator: line.to_string(),
                header: header.to_string(),
                line: at + 1,
                bullets: Vec::new(),
            });
            continue;
        }
        let Some(entry) = out.last_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("- ") {
            entry.bullets.push(Bullet {
                text: rest.trim().to_string(),
                items: 0,
                link: rest.contains("http://") || rest.contains("https://"),
                line: at + 1,
            });
        } else if let Some(b) = entry.bullets.last_mut() {
            let t = line.trim_start();
            if t.starts_with("* ") || t.starts_with("- ") {
                b.items += 1;
            }
            b.link |= t.contains("http://") || t.contains("https://");
            if !t.is_empty() && !t.starts_with("* ") && !t.starts_with("- ") {
                b.text.push(' ');
                b.text.push_str(t);
            }
        }
    }
    out
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const DATE_PATTERN: &str = r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) {1,2}(\d{1,2}) (\d{2}):(\d{2}):(\d{2}) UTC (\d{4}) - ";

/// The whole header: the date, then `Name <mail>`.
fn header_re() -> Regex {
    Regex::new(&format!("{DATE_PATTERN}.+ <[^<>\\s@]+@[^<>\\s]+>$")).expect("a fixed pattern")
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The time of an entry header, in seconds since the epoch.
pub(crate) fn header_time(header: &str) -> Option<i64> {
    let c = Regex::new(DATE_PATTERN).ok()?.captures(header)?;
    let n = |i: usize| c[i].parse::<i64>().ok();
    let month = MONTHS.iter().position(|m| *m == &c[2])? as i64 + 1;
    let days = days_from_civil(n(7)?, month, n(3)?);
    Some(days * 86400 + n(4)? * 3600 + n(5)? * 60 + n(6)?)
}

/// The time of `2026-11-03T11:14:44`.
fn iso_time(text: &str) -> Option<i64> {
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let days = days_from_civil(d.next()??, d.next()??, d.next()??);
    Some(days * 86400 + t.next()?? * 3600 + t.next()?? * 60 + t.next()??)
}

/// The version an `Update to X` bullet names.
pub(crate) fn update_target(bullet: &str) -> Option<String> {
    let rest = bullet.strip_prefix("Update to ")?.trim_start();
    let rest = rest.strip_prefix("version ").unwrap_or(rest);
    let word = rest.split_whitespace().next()?;
    let word = word.trim_end_matches([',', ':', ';', '.']);
    Some(word.strip_prefix('v').unwrap_or(word).to_string())
}

/// Whether `word` is a version: a digit first, after an optional `v`.
pub(crate) fn looks_like_version(word: &str) -> bool {
    word.strip_prefix('v')
        .unwrap_or(word)
        .starts_with(|c: char| c.is_ascii_digit())
}

/// A version as rpm compares it: lower case, no `v`, no `+git...`-style
/// suffix, and `-`, `_` and `~` all the separator `.`. A run of letters and a
/// run of digits are parts of their own, so `1.0-rc1`, `1.0~rc1`, `1.0rc1` and
/// `1.0.rc.1` are one release.
fn normalised(version: &str) -> String {
    let v = version.to_ascii_lowercase();
    let v = v.strip_prefix('v').unwrap_or(&v);
    let v = v.split('+').next().unwrap_or(v);
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in v.chars() {
        let c = if matches!(c, '-' | '_' | '~') { '.' } else { c };
        if let Some(p) = prev
            && p != '.'
            && c != '.'
            && p.is_ascii_digit() != c.is_ascii_digit()
        {
            out.push('.');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// How a bullet's version differs from the Version: `None` when they are the
/// same release, `Some(false)` when the bullet is a prefix of it (`2.10` for
/// `2.10.4`) or the other way round, `Some(true)` when they are different.
pub(crate) fn mismatch(target: &str, version: &str) -> Option<bool> {
    let (t, v) = (normalised(target), normalised(version));
    if t == v {
        return None;
    }
    let related = |a: &str, b: &str| a.strip_prefix(b).is_some_and(|r| r.starts_with('.'));
    Some(!(related(&t, &v) || related(&v, &t)))
}

const DEPENDENCY_VERBS: [&str; 7] = ["add", "drop", "raise", "relax", "require", "bump", "remove"];
const DEPENDENCY_WORDS: [&str; 7] = [
    "buildrequires",
    "requires",
    "dependency",
    "dependencies",
    ">=",
    "or newer",
    "upper bound",
];

/// Whether a bullet is a dependency change, which convention 40 gives no
/// bullet: it starts with a dependency verb and speaks of a requirement.
pub(crate) fn is_dependency_bullet(text: &str) -> bool {
    let verb = text.split_whitespace().next().unwrap_or("").to_lowercase();
    DEPENDENCY_VERBS.contains(&verb.as_str())
        && DEPENDENCY_WORDS
            .iter()
            .any(|w| text.to_lowercase().contains(w))
}

/// Whether a bullet holds `TODO`, a macro or the package name (convention 40).
pub(crate) fn has_forbidden_text(text: &str, package: &str) -> bool {
    let by_name = text
        .split(|ch: char| !(ch.is_alphanumeric() || "-_+.".contains(ch)))
        .any(|w| w.eq_ignore_ascii_case(package));
    text.contains("TODO") || text.contains("%{") || by_name
}

/// The findings for the `.changes` entries the request adds.
pub fn changes_findings(c: &ChangesCtx<'_>) -> Vec<Finding> {
    let mk = |id, sev, msg: &str, line: Option<usize>| {
        Finding::check(id, sev, c.package, msg).in_file(c.file, line)
    };
    let mut out = Vec::new();
    let (added, suffix) = match c.new {
        Some(new) => added_text(c.base, new),
        None => ("", true),
    };
    let new_entries = entries(added);
    if new_entries.is_empty() {
        if c.packaging_changed {
            out.push(mk(
                ids::CHANGES_MISSING_ENTRY,
                Severity::Blocker,
                "the spec or a patch changed and the request adds no entry to the .changes",
                None,
            ));
        }
        return out;
    }
    if !suffix {
        out.push(mk(
            ids::CHANGES_OLD_ENTRIES_EDITED,
            Severity::Warning,
            "the old entries of the .changes are not kept as they were",
            None,
        ));
    }
    let re = header_re();
    for e in &new_entries {
        if e.separator.len() != 67 || !re.is_match(&e.header) {
            out.push(mk(
                ids::CHANGES_HEADER_FORMAT,
                Severity::Warning,
                &format!(
                    "the entry header {:?} is not `Www Mmm [ d]d HH:MM:SS UTC YYYY - Name <mail>` \
                     under a line of 67 dashes",
                    e.header
                ),
                Some(e.line),
            ));
        }
    }
    let newest = &new_entries[0];
    if let Some(at) = header_time(&newest.header) {
        let previous = c
            .base
            .and_then(|b| entries(b).first().and_then(|e| header_time(&e.header)));
        if previous.is_some_and(|p| at < p) {
            out.push(mk(
                ids::CHANGES_DATE_ORDER,
                Severity::Warning,
                "the new entry is older than the one before it",
                Some(newest.line),
            ));
        } else if iso_time(c.created).is_some_and(|created| at > created + 86400) {
            out.push(mk(
                ids::CHANGES_DATE_ORDER,
                Severity::Warning,
                "the new entry is dated after the request was created",
                Some(newest.line),
            ));
        }
    }
    let bullets: Vec<&Bullet> = new_entries.iter().flat_map(|e| &e.bullets).collect();
    let updates: Vec<(&Bullet, String)> = bullets
        .iter()
        .filter_map(|b| Some((*b, update_target(&b.text)?)))
        .collect();
    let changed_version = c.base_version.is_some_and(|b| b != c.new_version);
    // Only a word that is a version (it starts with a digit) is compared:
    // `Update to git snapshot 2024...` names no version to be wrong about.
    let versions: Vec<&(&Bullet, String)> = updates
        .iter()
        .filter(|(_, t)| looks_like_version(t))
        .collect();
    if changed_version {
        let from = c.base_version.unwrap_or_default();
        if updates.is_empty() {
            out.push(mk(
                ids::CHANGES_VERSION_MISMATCH,
                Severity::Warning,
                &format!(
                    "Version changed from {from} to {} and no new bullet reads `Update to {}`",
                    c.new_version, c.new_version
                ),
                Some(newest.line),
            ));
        }
        for (b, target) in versions {
            let Some(clearly) = mismatch(target, c.new_version) else {
                continue;
            };
            // A bullet that names another release is wrong; one that names the
            // same release in a form rpm spells differently, or a shorter one, is
            // a slip.
            let severity = if clearly {
                Severity::Blocker
            } else {
                Severity::Warning
            };
            out.push(mk(
                ids::CHANGES_VERSION_MISMATCH,
                severity,
                &format!(
                    "the bullet says `Update to {target}` but the Version is {}",
                    c.new_version
                ),
                Some(b.line),
            ));
        }
    } else {
        for (b, target) in versions {
            out.push(mk(
                ids::CHANGES_UPDATE_WITHOUT_VERSION_CHANGE,
                Severity::Warning,
                &format!("a bullet says `Update to {target}` but the Version is unchanged"),
                Some(b.line),
            ));
        }
    }
    for (b, target) in &updates {
        if b.items == 0 && !b.link {
            out.push(mk(
                ids::CHANGES_NO_UPSTREAM_NOTES,
                Severity::Warning,
                &RiskFlag::NoUpstreamNotes {
                    version: target.clone(),
                }
                .line(),
                Some(b.line),
            ));
        }
    }
    for b in bullets.iter().filter(|b| update_target(&b.text).is_none()) {
        if is_dependency_bullet(&b.text) {
            out.push(mk(
                ids::CHANGES_DEPENDENCY_BULLET,
                Severity::Warning,
                &format!(
                    "a dependency change gets no bullet (convention 40): `{}`",
                    b.text
                ),
                Some(b.line),
            ));
        }
        if has_forbidden_text(&b.text, c.package) {
            out.push(mk(
                ids::CHANGES_FORBIDDEN_TEXT,
                Severity::Note,
                &format!(
                    "a bullet holds TODO, a macro or the package name (convention 40): `{}`",
                    b.text
                ),
                Some(b.line),
            ));
        }
    }
    for e in &new_entries {
        let refreshed: Vec<&str> = e
            .bullets
            .iter()
            .filter_map(|b| b.text.strip_prefix("Refresh "))
            .filter(|n| !n.starts_with("patches"))
            .filter_map(|n| n.split_whitespace().next())
            .collect();
        if refreshed.len() >= 2 {
            out.push(mk(
                ids::CHANGES_REFRESH_NOT_GROUPED,
                Severity::Warning,
                &format!(
                    "{} Refresh bullets in one entry; group them as `{}`",
                    refreshed.len(),
                    crate::patch_check::refresh_bullet(&refreshed).replace('\n', " ")
                ),
                Some(e.line),
            ));
        }
    }
    let words: Vec<&str> = added.split_whitespace().collect();
    for (verb, names) in [("added", c.patches_added), ("removed", c.patches_removed)] {
        for name in names
            .iter()
            .filter(|n| !crate::pipeline::names_in_bullet(&words, n))
        {
            out.push(mk(
                ids::CHANGES_PATCH_NOT_NAMED,
                Severity::Warning,
                &format!("{name} was {verb} and no new bullet names it"),
                None,
            ));
        }
    }
    out
}
