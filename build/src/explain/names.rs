//! Finding a package in a work directory by the name the user typed. The rule
//! is the one `--only` uses ([`osc::project_key`]: case and separators do not
//! count), read through [`osc::find_listed`]: an entry that differs from the
//! request only in case is the same package, and two entries that differ only
//! in separators are not guessed between.

use std::collections::BTreeSet;
use std::path::Path;

use ap_core::event::Event;
use serde_json::Value;

use crate::osc::{Listed, find_listed, fuzzy_key, project_key};

use super::tries::PkgEvent;

/// The names of the sub-directories of `dir`, sorted; none when unreadable.
pub(super) fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The stem of the package notes file of `pkg` in `notes_dir` (`<stem>.md`),
/// the one that is `pkg` up to case and separators, as [`spelling`] reads it.
pub fn package_notes_name(notes_dir: &Path, pkg: &str) -> Result<Option<String>, String> {
    let names: Vec<String> = std::fs::read_dir(notes_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".md").map(str::to_string)
        })
        .collect();
    spelling(pkg, &names)
}

/// The entry of `names` that is `pkg` by the `--only` rule: the exact spelling,
/// else the one that is `pkg` in lowercase, else the only spelling. `Err` when
/// several entries are the same project up to separators: the caller lists
/// them and asks for the exact spelling.
pub fn spelling(pkg: &str, names: &[String]) -> Result<Option<String>, String> {
    match find_listed(pkg, names.iter().map(String::as_str)) {
        Listed::Found(name) => Ok(Some(name.to_string())),
        Listed::Absent => Ok(None),
        Listed::Ambiguous(same) => Err(format!(
            "{pkg} matches {} entries that are the same project up to separators ({}); pass the exact spelling",
            same.len(),
            same.join(", ")
        )),
    }
}

/// Whether `names` holds `pkg` up to case and separators, an ambiguous set
/// included (the lookup that follows reports it).
pub(super) fn names_match(pkg: &str, names: &[String]) -> bool {
    !matches!(
        find_listed(pkg, names.iter().map(String::as_str)),
        Listed::Absent
    )
}

/// The note for entries that are `pkg` in another case only, when there are
/// more than one: which one the report reads.
pub(super) fn case_copies(what: &str, pkg: &str, names: &[String], chosen: &str) -> Option<String> {
    let same: BTreeSet<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| n.to_lowercase() == pkg.to_lowercase())
        .collect();
    (same.len() > 1).then(|| {
        let all: Vec<&str> = same.into_iter().collect();
        format!(
            "{what} holds {} entries that are {pkg} up to case ({}); the report reads {chosen}",
            all.len(),
            all.join(", ")
        )
    })
}

/// Whether a `run.jsonl` line may name `pkg`, up to case and separators: the
/// name without separators or case occurs in it. A cheap test before parsing;
/// every spelling of the owner contains it too.
pub(super) fn may_name_package(line: &str, pkg: &str) -> bool {
    fuzzy_key(line).contains(&fuzzy_key(pkg))
}

/// Whether a `run.jsonl` line is an event of `pkg` (its `pkg`, or the
/// `package` of an `arch_check`), up to case and separators.
pub(super) fn names_package(line: &str, pkg: &str) -> bool {
    if !may_name_package(line, pkg) {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    [v["pkg"].as_str(), v["package"].as_str()]
        .into_iter()
        .flatten()
        .any(|o| project_key(o) == project_key(pkg))
}

/// The name OBS lists `pkg` under: the state's `obs_name`, else the spelling
/// the run's events carry (`pkg` itself when it is among them), else `pkg`.
pub(super) fn canonical_name(pkg: &str, state: Option<&Value>, records: &[PkgEvent]) -> String {
    if let Some(o) = state
        .and_then(|s| s["obs_name"].as_str())
        .filter(|o| project_key(o) == project_key(pkg))
    {
        return o.to_string();
    }
    let carried: BTreeSet<&str> = records
        .iter()
        .map(|(ctx, ev)| match ev {
            Event::PackageFinished { pkg, .. } => pkg.as_str(),
            _ => ctx.as_str(),
        })
        .filter(|p| project_key(p) == project_key(pkg))
        .collect();
    if carried.contains(pkg) {
        return pkg.to_string();
    }
    carried
        .first()
        .map_or_else(|| pkg.to_string(), |p| (*p).to_string())
}

/// Spells the events of `pkg` in `records` as `canonical`, so that they are
/// counted as one package whatever case or separators they were written in.
pub(super) fn canonicalise(records: &mut [PkgEvent], pkg: &str, canonical: &str) {
    let key = project_key(pkg);
    for (ctx, ev) in records.iter_mut() {
        if project_key(ctx) == key {
            *ctx = canonical.to_string();
        }
        if let Event::PackageFinished { pkg: p, .. } = ev
            && project_key(p) == key
        {
            *p = canonical.to_string();
        }
    }
}
