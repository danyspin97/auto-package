//! The bullets and blocks of an unpublished entry's body, as the
//! normaliser reads them: a bullet with its continuation lines, and the lines
//! of an entry split into the release's bullets, the older releases' and the
//! others.

use super::changes_normalise::is_older;
use crate::request_review::changes_checks::update_target;
use crate::version::Scheme;

/// One bullet of an entry body: the lines as written, the first without its
/// `- `.
pub(super) struct Bullet {
    pub(super) lines: Vec<String>,
}

impl Bullet {
    pub(super) fn head(&self) -> &str {
        self.lines[0].trim()
    }

    /// The bullet's text, its continuation lines joined on it.
    pub(super) fn text(&self) -> String {
        let mut text = self.head().to_string();
        for l in &self.lines[1..] {
            text.push(' ');
            text.push_str(l.trim());
        }
        text
    }

    /// The patch a `Refresh <patch>` bullet names, `None` for any other: a
    /// single name that is a patch file (`.patch` or `.diff`).
    pub(super) fn refreshed(&self) -> Option<&str> {
        let name = self.head().strip_prefix("Refresh ")?.trim();
        let patch = name.ends_with(".patch") || name.ends_with(".diff");
        (self.lines.len() == 1 && patch && !name.contains(char::is_whitespace)).then_some(name)
    }

    /// The patches an already grouped `Refresh patches:` bullet lists.
    pub(super) fn grouped(&self) -> Option<Vec<&str>> {
        (self.head() == "Refresh patches:").then(|| {
            self.lines[1..]
                .iter()
                .filter_map(|l| l.trim().strip_prefix("* "))
                .map(str::trim)
                .collect()
        })
    }

    /// The continuation lines of a grouped `Refresh patches:` bullet that are
    /// not `* ` items, kept after the group as they are written.
    pub(super) fn group_notes(&self) -> Vec<String> {
        if self.head() != "Refresh patches:" {
            return Vec::new();
        }
        self.lines[1..]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("* "))
            .map(|l| format!("  {l}"))
            .collect()
    }
}

/// The index of the first bullet of an entry: the separator, the header line
/// and the blank lines before the bullets come before it. The entry's length
/// when it has no bullet.
pub(super) fn body_start(entry: &[&str]) -> usize {
    entry
        .iter()
        .skip(2)
        .position(|l| l.starts_with("- "))
        .map_or(entry.len(), |i| i + 2)
}

/// The lines of an entry's body (from its first bullet), split in three: the
/// `Update to` bullets for `version` (and any other `Update to`), the `Update
/// to` bullets for an older release than `version`, and the others. A bullet's
/// continuation lines go with it.
pub(super) fn split_blocks(
    entry: &[&str],
    version: &str,
    scheme: Scheme,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut current, mut older, mut other) = (Vec::new(), Vec::new(), Vec::new());
    let mut section = 2;
    for line in &entry[body_start(entry)..] {
        if let Some(rest) = line.strip_prefix("- ") {
            section = match update_target(rest.trim()) {
                Some(t) if is_older(&t, version, scheme) => 1,
                Some(_) => 0,
                None => 2,
            };
        }
        match section {
            0 => current.push(line.to_string()),
            1 => older.push(line.to_string()),
            _ => other.push(line.to_string()),
        }
    }
    (current, older, other)
}
