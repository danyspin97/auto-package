//! The version bounds a spec puts on the names a package waits on. OBS
//! names only the ones it reports; the spec may ask for more (a translated
//! turn raises `Requires: x >= 2026.7` while OBS reports the bare name).

use super::rpm_names::same_package;

/// The bounds the spec's `Requires` and `BuildRequires` lines put on each of
/// `deps`, as `(dep, version)`: `Requires: python3-x >= 2026.7` bounds `x`.
/// Only the spaced form `name OP version` is read.
pub(super) fn spec_bounds(spec: &str, deps: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in spec.lines() {
        let Some((tag, list)) = line.trim().split_once(':') else {
            continue;
        };
        if !matches!(tag.trim(), "Requires" | "BuildRequires") {
            continue;
        }
        let words: Vec<&str> = list.split_whitespace().collect();
        for (i, name) in words.iter().enumerate() {
            let (Some(op), Some(version)) = (words.get(i + 1), words.get(i + 2)) else {
                continue;
            };
            if !matches!(*op, ">=" | "==" | "=") {
                continue;
            }
            for dep in deps.iter().filter(|d| same_package(name, d)) {
                let entry = (dep.clone(), version.to_string());
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
    }
    out
}
