//! The provenance audit of `lint`: where each carried patch that an upstream
//! request copies comes from, and what that request is now (plan 4.4). It only
//! reads; tidy writes the header, and the pipeline acts on the verdicts.

use super::*;
use ap_build::patch_provenance::{self, Inputs, LiveForge};

/// The findings for the patches of `pkg`'s spec, read from `source`. A patch
/// or the `.changes` file that cannot be read is a warning; the audit goes on.
pub(super) fn provenance_findings(
    source: &str,
    pkg: &str,
    spec: &str,
    emit: Emit<'_>,
    warn: &dyn Fn(String),
    hosts: &ap_build::allowed_hosts::AllowedHosts,
) -> Vec<quality::Finding> {
    let web = web_patch_numbers(spec);
    let mut patches: Vec<(String, String)> = Vec::new();
    for (number, name) in ap_build::patch_check::declared_patches(spec) {
        if web.contains(&number) || name.contains('/') {
            continue;
        }
        match osc::cat_file(source, pkg, &name, emit) {
            Some(text) => patches.push((name, text)),
            None => warn(format!(
                "could not read {name} of {pkg}, so its upstream provenance is not checked"
            )),
        }
    }
    if patches.is_empty() {
        return Vec::new();
    }
    let changes = changes_text(source, pkg, emit).unwrap_or_default();
    let Some(version) = ap_build::spec::parse_header(spec).map(|h| h.version) else {
        return Vec::new();
    };
    let inputs = Inputs {
        pkg,
        spec,
        changes: &changes,
        version: &version,
        repo: ap_build::bugfix::upstream_repo_of_spec(spec),
        patches: &patches,
    };
    let forge = LiveForge {
        package: pkg,
        hosts,
    };
    patch_provenance::lint_findings(&patch_provenance::assess(
        &inputs,
        &forge,
        ap_build::upstream_cache::refreshing(),
    ))
}

/// The numbers of the `PatchN:` lines whose value is a URL: their file is not
/// in the package, so they are not read.
fn web_patch_numbers(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(|line| {
            let (tag, value) = line.split_once(':')?;
            let number = tag.trim().strip_prefix("Patch")?;
            let number = number.trim_end_matches(char::is_whitespace);
            (number.bytes().all(|b| b.is_ascii_digit()) && value.contains("://"))
                .then(|| number.to_string())
        })
        .collect()
}

/// The package's `.changes`: its own name, else the only one it has.
fn changes_text(source: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    if let Some(text) = osc::cat_file(source, pkg, &format!("{pkg}.changes"), emit) {
        return Some(text);
    }
    let files = osc::list_package_files(source, pkg, emit)?;
    let mut changes = files.iter().filter(|f| f.ends_with(".changes"));
    let only = changes.next()?;
    if changes.next().is_some() {
        return None;
    }
    osc::cat_file(source, pkg, only, emit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_patch_is_not_read_from_the_package() {
        let spec = "Name: x\nPatch0: 106.patch\nPatch1: https://github.com/o/r/pull/7.patch#/7.patch\nPatch2: 7-fix.patch\n";
        assert_eq!(web_patch_numbers(spec), ["1"]);
    }
}
