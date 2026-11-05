//! The provenance audit of `lint`: where each carried patch that an upstream
//! request copies comes from, and what that request is now (plan 4.4). It only
//! reads; tidy writes the header, and the pipeline acts on the verdicts.

use super::*;
use ap_build::patch_provenance::{self, Inputs, LiveForge};
use std::path::Path;

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
    let mut patches: Vec<(String, String)> = Vec::new();
    for name in plain_patch_names(spec) {
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
    patch_provenance::lint_findings(&assess_patches(pkg, spec, &changes, &patches, hosts))
}

/// The provenance of the patches (name, text) of `spec`, assessed against the
/// forge. Empty when the spec has no version.
pub(super) fn assess_patches(
    pkg: &str,
    spec: &str,
    changes: &str,
    patches: &[(String, String)],
    hosts: &ap_build::allowed_hosts::AllowedHosts,
) -> Vec<patch_provenance::Provenance> {
    let Some(version) = ap_build::spec::parse_header(spec).map(|h| h.version) else {
        return Vec::new();
    };
    let inputs = Inputs {
        pkg,
        spec,
        changes,
        version: &version,
        repo: ap_build::bugfix::upstream_repo_of_spec(spec),
        patches,
    };
    let forge = LiveForge {
        package: pkg,
        hosts,
    };
    patch_provenance::assess(&inputs, &forge, ap_build::upstream_cache::refreshing())
}

/// The plain file names (not URLs) of the patches `spec` declares.
pub(super) fn plain_patch_names(spec: &str) -> Vec<String> {
    let web = web_patch_numbers(spec);
    ap_build::patch_check::declared_patches(spec)
        .into_iter()
        .filter(|(number, name)| !web.contains(number) && !name.contains('/'))
        .map(|(_, name)| name)
        .collect()
}

/// What tidy writes for the provenance of a checkout: the spec with the
/// `PATCH-FIX-UPSTREAM` comments, and each patch whose header changes as
/// `(name, before, after)`.
pub(super) struct ProvenanceEdits {
    pub(super) spec: String,
    pub(super) patches: Vec<(String, String, String)>,
}

/// The provenance edits for the checkout of `pkg`: `spec` is the spec after
/// the spec tidy, `patches` the plain patch files of the checkout, `changes`
/// the `.changes` text. Nothing is edited when no edit is due.
pub(super) fn header_edits(
    pkg: &str,
    spec: &str,
    patches: &[(String, String)],
    changes: &str,
    hosts: &ap_build::allowed_hosts::AllowedHosts,
) -> ProvenanceEdits {
    let provenance = assess_patches(pkg, spec, changes, patches, hosts);
    edits_of(&provenance, patches, spec)
}

/// The edits the assessed patches are due (pure: no network): each header of
/// `patches` that gets lines, and the spec with one comment per patch that
/// needs one.
pub(super) fn edits_of(
    provenance: &[patch_provenance::Provenance],
    patches: &[(String, String)],
    spec: &str,
) -> ProvenanceEdits {
    let mut out = ProvenanceEdits {
        spec: spec.to_string(),
        patches: Vec::new(),
    };
    for p in provenance {
        if let Some((name, before)) = patches.iter().find(|(n, _)| *n == p.name)
            && let Some(after) = patch_provenance::header_for(p, before)
        {
            out.patches.push((name.clone(), before.clone(), after));
        }
        if let Some(spec) = patch_provenance::spec_comment_for(p, &out.spec) {
            out.spec = spec;
        }
    }
    out
}

/// The plain patch files of a checkout, read from `dir`; a missing one is left out.
pub(super) fn checkout_patches(dir: &Path, spec: &str) -> Vec<(String, String)> {
    plain_patch_names(spec)
        .into_iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(dir.join(&name)).ok()?;
            Some((name, text))
        })
        .collect()
}

/// The `.changes` text of a checkout: the package's own, else its only one,
/// else empty.
pub(super) fn checkout_changes(dir: &Path, pkg: &str, files: &[String]) -> String {
    let own = format!("{pkg}.changes");
    let name = if files.contains(&own) {
        own
    } else {
        let mut all = files.iter().filter(|f| f.ends_with(".changes"));
        match (all.next(), all.next()) {
            (Some(only), None) => only.clone(),
            _ => return String::new(),
        }
    };
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
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

    #[test]
    fn a_header_and_a_comment_are_the_only_edits_of_a_file() {
        use ap_build::patch_provenance::{
            HeaderPlan, OriginEvidence, OriginKind, PatchOrigin, Provenance, Verdict,
        };
        let spec = "Name: x\nVersion: 1\nURL: https://github.com/o/r\nPatch0: 106.patch\n";
        let patch =
            "Subject: [PATCH] x\n\n---\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n".to_string();
        let p = Provenance {
            name: "106.patch".into(),
            origin: Some(PatchOrigin {
                url: "https://github.com/o/r/pull/106".into(),
                kind: OriginKind::Pr,
                project: "https://github.com/o/r".into(),
                number: Some(106),
                commit: None,
                evidence: OriginEvidence::ChangesBullet,
            }),
            has_upstream_line: false,
            status: Err("x".into()),
            containment: None,
            verdict: Verdict::Nothing,
            header: Some(HeaderPlan {
                status: Some("submitted"),
                commit: None,
                reason: Some("Fixes it".into()),
            }),
            spec_comment_due: true,
        };
        let patches = vec![("106.patch".to_string(), patch.clone())];
        let edits = edits_of(&[p], &patches, spec);
        assert_eq!(edits.patches.len(), 1);
        assert!(
            edits.patches[0]
                .2
                .contains("Upstream: submitted https://github.com/o/r/pull/106\n")
        );
        assert!(
            edits
                .spec
                .contains("# PATCH-FIX-UPSTREAM 106.patch gh#o/r#106 -- Fixes it\nPatch0")
        );
        assert_eq!(edits.patches[0].1, patch);
    }
}
