//! The quilt series a RefreshPatches turn reads: the spec's patches in rpm's order
//! and levels, written for one round and removed with it.

use super::*;

/// The scratch series of one refresh round, removed when it drops.
pub(super) struct RebaseSeries(PathBuf);

impl Drop for RebaseSeries {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes the spec's patches, in the order and with the levels rpm applies
/// them, as the quilt series the refresh turn reads (`QUILT_SERIES`). quilt
/// reads a series only from `QUILT_PATCHES`, the checkout, so the file sits
/// there under [`crate::agent::REFRESH_SERIES`] and is removed when the guard
/// drops. The checkout's own `series` is never written. The returned lines
/// say where that series disagrees with the spec.
pub(super) fn write_rebase_series(spec: &str, checkout: &Path) -> (RebaseSeries, Vec<String>) {
    let entries: Vec<crate::patch_check::SeriesEntry> = crate::patch_check::plan_series(spec)
        .into_iter()
        .filter_map(|p| match p {
            crate::patch_check::Planned::Check(e) => Some(e),
            crate::patch_check::Planned::Skip { .. } => None,
        })
        .collect();
    let guard = RebaseSeries(checkout.join(crate::agent::REFRESH_SERIES));
    write_or_warn(&guard.0, crate::patch_check::series_text(&entries));
    let existing = std::fs::read_to_string(checkout.join("series")).unwrap_or_default();
    let notes = series_disagreements(&existing, &entries, |n| checkout.join(n).exists());
    (guard, notes)
}

/// What the checkout's `series` says that the spec's patches do not: a line
/// for a patch the spec does not apply, a level other than the spec's, a file
/// the checkout does not have, and a spec patch the series does not list.
pub(super) fn series_disagreements(
    current: &str,
    entries: &[crate::patch_check::SeriesEntry],
    present: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    let mut listed: Vec<&str> = Vec::new();
    for line in current.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut toks = t.split_whitespace();
        let name = toks.next().unwrap_or_default();
        let level = toks
            .find_map(|tok| tok.strip_prefix("-p")?.parse::<u32>().ok())
            .unwrap_or(1);
        listed.push(name);
        match entries.iter().find(|e| e.name == name) {
            None => notes.push(format!(
                "the checkout's series names {name}, which the spec does not apply"
            )),
            Some(e) if e.level != level => notes.push(format!(
                "the checkout's series applies {name} with -p{level}, the spec with -p{}",
                e.level
            )),
            Some(_) => {}
        }
        if !present(name) {
            notes.push(format!(
                "the checkout's series names {name}, which the checkout does not have"
            ));
        }
    }
    for e in entries {
        if !listed.contains(&e.name.as_str()) && !current.trim().is_empty() {
            notes.push(format!(
                "the checkout's series does not list {}, which the spec applies",
                e.name
            ));
        }
    }
    notes
}
