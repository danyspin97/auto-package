//! The one-line summary of a `.changes` entry that goes into the `osc ci`
//! message.

use super::commit::one_line;

/// A bullet that opens with the words a blocked turn is recorded under
/// (`a fix turn answered blocked: ...`, `agent blocked: ...`) is failure text,
/// not a changelog line. Flag lines are already left out by the caller, so the
/// check is anchored at the start of a bullet: a real bullet that only mentions
/// such words later (`Fix tests that cannot run because of X`) is kept. A real
/// bullet that begins with `agent blocked` is not expected.
fn looks_like_failure(bullet: &str) -> bool {
    let lower = bullet.trim_start().to_ascii_lowercase();
    ["a fix turn answered blocked", "agent blocked"]
        .iter()
        .any(|marker| lower.starts_with(marker))
}

/// The real bullets of the newest entry of `working` joined on one line and
/// cut at 100 characters on a word boundary. The flag lines of the entry, and
/// the lines they wrap onto, are not bullets and are left out. `None` when
/// that entry is empty, is already in `pristine`, or the summary would read
/// as a failure explanation.
pub(super) fn changes_summary(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    if pristine.contains(lines[first..end].join("\n").trim_end()) {
        return None;
    }
    let body = &lines[(first + 2).min(end)..end];
    let flag = crate::osc::flag_line_mask(body);
    let mut bullets: Vec<String> = Vec::new();
    for (l, _) in body.iter().zip(&flag).filter(|(_, f)| !**f) {
        if let Some(b) = l.strip_prefix("- ") {
            bullets.push(b.trim().to_string());
        } else if l.starts_with("  ")
            && !l.trim().is_empty()
            && let Some(last) = bullets.last_mut()
        {
            last.push(' ');
            last.push_str(l.trim());
        }
    }
    if bullets.iter().any(|b| looks_like_failure(b)) {
        return None;
    }
    let joined = one_line(&bullets.join("; "));
    if joined.is_empty() {
        return None;
    }
    let mut out: String = joined.chars().take(100).collect();
    if joined.chars().count() > 100 {
        out = out
            .rsplit_once(' ')
            .map_or(out.clone(), |(h, _)| h.to_string());
        out = out.trim_end_matches([',', ';', ':', ' ']).to_string();
    }
    Some(out)
}
