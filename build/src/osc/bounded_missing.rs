//! Bounds OBS names without saying what the repository has: a `nothing
//! provides NAME >= VERSION` line with no `(got version ...)` next to it.

use super::*;
use crate::osc::spec_info::*;

/// The `(name, required version)` of each `nothing provides NAME OP VERSION`
/// line in `detail` (`OP` one of `>=`, `==`, `=`) that carries no `(got
/// version ...)`. Whether the repository has the name at an older version, or
/// not at all, is for the caller to find out.
pub fn bounded_missing(detail: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (i, line) in detail.iter().enumerate() {
        let line = line.trim().trim_end_matches(',').trim();
        let Some(rest) = line.strip_prefix("nothing provides ") else {
            continue;
        };
        let next = detail.get(i + 1).map(|n| n.trim()).unwrap_or_default();
        if line.contains("(got version") || next.starts_with("(got version") {
            continue;
        }
        let Some((name, constraint)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let required = [">=", "==", "="].iter().find_map(|op| {
            constraint
                .trim()
                .strip_prefix(op)
                .map(|r| r.trim().to_string())
        });
        let Some(required) = required else {
            continue;
        };
        let version = required
            .split(['-', ' ', ','])
            .next()
            .unwrap_or_default()
            .to_string();
        if version.parse::<pep440_rs::Version>().is_err() {
            continue;
        }
        let Some(bare) = missing_package_names(&[name.to_string()])
            .into_iter()
            .next()
        else {
            continue;
        };
        if !out.iter().any(|(n, _)| fuzzy_key(n) == fuzzy_key(&bare)) {
            out.push((bare, version));
        }
    }
    out
}

/// Adds to `outdated` the bounds of [`bounded_missing`] for the names in
/// `to_build` that the repository lists (`own`). A name it does not list is
/// absent, not outdated, and is built as before.
pub fn add_bounded_outdated(
    outdated: &mut Vec<(String, Option<String>)>,
    detail: &[String],
    to_build: &[String],
    own: Option<&std::collections::HashSet<String>>,
) {
    let Some(own) = own else {
        return;
    };
    for (name, required) in bounded_missing(detail) {
        let key = fuzzy_key(&name);
        let listed = own.iter().any(|p| {
            let k = fuzzy_key(p);
            k == fuzzy_key(&rpm_name(&name)) || k == key
        });
        let wanted = to_build.iter().any(|b| fuzzy_key(b) == key);
        if listed && wanted && !outdated.iter().any(|(o, _)| fuzzy_key(o) == key) {
            outdated.push((name, Some(required)));
        }
    }
}
