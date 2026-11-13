//! The test paths a `%check` names that the source does not ship. A path one of
//! the spec's patches creates counts as shipped, and on an update a problem the
//! origin's `%check` has too is inherited, not introduced by the edit.

use crate::pipeline::patch_check::*;
use std::path::Path;

/// Drops `level` leading components of a diff name (`-pN`), `None` when the name
/// has fewer.
fn strip_components(name: &str, level: u32) -> Option<&str> {
    let mut rest = name;
    for _ in 0..level {
        rest = rest.split_once('/')?.1;
    }
    Some(rest).filter(|r| !r.is_empty())
}

/// The paths a diff creates: a `+++` name whose `---` line right before it is
/// `/dev/null`, with `level` components stripped. A deleted file is not one.
pub(crate) fn created_paths(diff: &str, level: u32) -> Vec<String> {
    let mut out = Vec::new();
    let mut old: Option<&str> = None;
    for line in diff.lines() {
        let prev = old.take();
        if let Some(rest) = line.strip_prefix("--- ") {
            old = Some(rest.split('\t').next().unwrap_or("").trim());
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let new = rest.split('\t').next().unwrap_or("").trim();
            if prev == Some("/dev/null") && new != "/dev/null" {
                out.extend(strip_components(new, level).map(str::to_string));
            }
        }
    }
    out
}

/// The paths the patches `spec` declares create, read from `checkout`. A patch
/// that is not a bare file name in the checkout, or cannot be read, creates none.
pub(crate) fn patch_created_paths(spec: &str, checkout: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for patch in spec
        .lines()
        .filter_map(|l| crate::spec::parse_source_line(l, &["patch"], str::to_string))
    {
        let name = expand_patch_name(&patch.value, spec);
        if Path::new(&name).file_name() != Some(name.as_ref()) {
            continue;
        }
        let level = spec_patch_level(spec, &patch.index.to_string());
        if let Ok(diff) = std::fs::read(checkout.join(&name)) {
            out.extend(created_paths(&String::from_utf8_lossy(&diff), level));
        }
    }
    out
}

/// The review problems of the `%check` paths `spec` names that `have` does not
/// find, as `(introduced, inherited)`. With an `origin` spec, a path its `%check`
/// names as missing too is inherited.
pub(crate) fn missing_check_path_split(
    spec: &str,
    origin: Option<&str>,
    have: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let missing = |s: &str| crate::spec::missing_check_paths(s, have);
    let now = if spec.contains("--wildcards") {
        Vec::new()
    } else {
        missing(spec)
    };
    let before = origin.map(missing).unwrap_or_default();
    let (inherited, introduced): (Vec<String>, Vec<String>) =
        now.into_iter().partition(|p| before.contains(p));
    let problems = |paths: &[String]| {
        if paths.is_empty() {
            Vec::new()
        } else {
            crate::spec::missing_paths_problem(paths)
        }
    };
    (problems(&introduced), problems(&inherited))
}
