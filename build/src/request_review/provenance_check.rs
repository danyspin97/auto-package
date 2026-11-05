//! The upstream provenance of the patches a request adds or changes (plan 4.9):
//! what each carried patch's upstream request is now, as notes. It reads the
//! forge and writes nothing in the review; a state it cannot read is skipped.

use super::gather::{Side, Skipped};
use super::ids;
use super::{Finding, Severity};
use crate::patch_provenance::{self as pp, ForgeNet, Verdict};

/// A forge that answers nothing: for a review that makes no forge call.
pub struct NoForge;

impl ForgeNet for NoForge {
    fn api_json(&self, _url: &str) -> Result<serde_json::Value, String> {
        Err("no forge is asked in this review".to_string())
    }
    fn patch_text(&self, _url: &str) -> Result<String, String> {
        Err("no forge is asked in this review".to_string())
    }
    fn release_tags(&self, _repo: &str) -> Result<Vec<String>, String> {
        Err("no forge is asked in this review".to_string())
    }
}

/// The [`NoForge`] of a review that asks no forge.
pub static NO_FORGE: NoForge = NoForge;

/// The notes and the skipped checks of the patches `new` declares that carry an
/// upstream request. Only a patch the request adds or changes gets the
/// `patch.provenance-missing` note.
pub fn provenance_findings(
    package: &str,
    base: &Side,
    new: &Side,
    net: &dyn ForgeNet,
) -> (Vec<Finding>, Vec<Skipped>) {
    let mut findings = Vec::new();
    let mut skipped = Vec::new();
    let Some(spec) = new.spec() else {
        return (findings, skipped);
    };
    let patches: Vec<(String, String)> = crate::patch_check::declared_patches(spec)
        .into_iter()
        .filter_map(|(_, name)| {
            let text = new.texts.get(&name)?.clone();
            Some((name, text))
        })
        .collect();
    if patches.is_empty() {
        return (findings, skipped);
    }
    let changes = new.changes().unwrap_or_default();
    let version = crate::spec::parse_header(spec)
        .map(|h| h.version)
        .unwrap_or_default();
    let inputs = pp::Inputs {
        pkg: package,
        spec,
        changes,
        version: &version,
        repo: crate::bugfix::upstream_repo_of_spec(spec),
        patches: &patches,
    };
    let provenance = pp::assess(&inputs, net, false);
    for p in &provenance {
        let changed = base.texts.get(&p.name) != new.texts.get(&p.name);
        let url = p
            .origin
            .as_ref()
            .map(|o| o.url.as_str())
            .unwrap_or_default();
        let name = p.name.as_str();
        let (id, message) = match &p.verdict {
            Verdict::Nothing => {
                if changed && pp::header_due(p) {
                    (
                        ids::PATCH_PROVENANCE_MISSING,
                        format!("{name} lacks the upstream lines of its request {url}"),
                    )
                } else {
                    continue;
                }
            }
            Verdict::Redundant { tag } => (
                ids::PATCH_UPSTREAM_MERGED,
                format!("{name}: {url} is merged and in {tag}"),
            ),
            Verdict::ReleasedLater { tag, .. } => (
                ids::PATCH_UPSTREAM_RELEASED,
                format!("{name}: {url} is merged in {tag}, after the packaged release"),
            ),
            Verdict::HeadMoved { .. } => (
                ids::PATCH_UPSTREAM_MOVED,
                format!("{name}: {url} moved with different code"),
            ),
            Verdict::Closed => (
                ids::PATCH_UPSTREAM_CLOSED,
                format!("{name}: {url} is closed without a merge"),
            ),
            Verdict::MovedNotComparable(why) | Verdict::NotChecked(why) => {
                skipped.push(Skipped {
                    check: "patch provenance".to_string(),
                    why: format!("{name}: {why}"),
                });
                continue;
            }
        };
        findings.push(Finding::check(id, Severity::Note, package, &message).in_file(name, None));
    }
    (findings, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch_provenance::status_tests::FakeForge;
    use serde_json::json;
    use std::collections::BTreeMap;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const SPEC: &str = "Name: x\nVersion: 1.0\nURL: https://github.com/o/r\n# https://github.com/o/r/pull/106\nPatch0: 106.patch\n";

    fn side(texts: &[(&str, &str)]) -> Side {
        Side {
            listing: Default::default(),
            texts: texts
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect::<BTreeMap<_, _>>(),
            spec_name: Some("x.spec".into()),
        }
    }

    fn patch() -> String {
        format!(
            "From {SHA} Mon Sep 17 00:00:00 2001\nSubject: [PATCH 1/1] x\n\nmsg\n---\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n"
        )
    }

    #[test]
    fn an_added_patch_of_an_open_request_gets_the_provenance_note() {
        let mut net = FakeForge::default();
        net.json.insert(
            "https://api.github.com/repos/o/r/pulls/106".into(),
            Ok(json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": "t"})),
        );
        let new = side(&[("x.spec", SPEC), ("106.patch", &patch())]);
        let (findings, skipped) = provenance_findings("x", &side(&[]), &new, &net);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::PATCH_PROVENANCE_MISSING);
        assert_eq!(findings[0].severity, Severity::Note);
    }

    #[test]
    fn a_forge_that_cannot_be_asked_is_a_skipped_check() {
        let new = side(&[("x.spec", SPEC), ("106.patch", &patch())]);
        let (findings, skipped) = provenance_findings("x", &side(&[]), &new, &NO_FORGE);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].check, "patch provenance");
    }

    fn verdict_ids(state: serde_json::Value, carried: &str) -> (Vec<&'static str>, usize) {
        let mut net = FakeForge {
            tags: vec!["v1.0".into()],
            ..Default::default()
        };
        net.json.insert(
            "https://api.github.com/repos/o/r/pulls/106".into(),
            Ok(state),
        );
        let new = side(&[("x.spec", SPEC), ("106.patch", carried)]);
        let (findings, skipped) = provenance_findings("x", &side(&[]), &new, &net);
        (findings.iter().map(|f| f.id).collect(), skipped.len())
    }

    #[test]
    fn each_provenance_verdict_maps_to_its_note_or_skip() {
        let carried = patch();
        let merged = json!({"state": "closed", "merged": true, "merge_commit_sha": SHA, "head": {"sha": SHA}, "title": "t"});
        // Merged and in the packaged release: the redundant note.
        let mut net = FakeForge {
            tags: vec!["v1.0".into()],
            ..Default::default()
        };
        net.json.insert(
            "https://api.github.com/repos/o/r/pulls/106".into(),
            Ok(merged),
        );
        net.json.insert(
            format!("https://api.github.com/repos/o/r/compare/v1.0...{SHA}"),
            Ok(json!({"status": "identical"})),
        );
        let new = side(&[("x.spec", SPEC), ("106.patch", &carried)]);
        let (findings, _) = provenance_findings("x", &side(&[]), &new, &net);
        assert_eq!(
            findings.iter().map(|f| f.id).collect::<Vec<_>>(),
            [ids::PATCH_UPSTREAM_MERGED]
        );

        let closed =
            json!({"state": "closed", "merged": false, "head": {"sha": SHA}, "title": "t"});
        assert_eq!(
            verdict_ids(closed, &carried).0,
            [ids::PATCH_UPSTREAM_CLOSED]
        );

        let moved = json!({"state": "open", "merged": false, "head": {"sha": "9999999999999999999999999999999999999999"}, "title": "t"});
        // The head moved and the carried file is a different revision: moved.
        let mut net = FakeForge {
            tags: vec!["v1.0".into()],
            ..Default::default()
        };
        net.json.insert(
            "https://api.github.com/repos/o/r/pulls/106".into(),
            Ok(moved),
        );
        net.patches.insert(
            "https://github.com/o/r/pull/106.patch".into(),
            format!("From 9999999999999999999999999999999999999999 Mon Sep 17 00:00:00 2001\nSubject: [PATCH 1/1] x\n\nmsg\n---\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+c\n"),
        );
        let (findings, _) = provenance_findings("x", &side(&[]), &new, &net);
        assert_eq!(
            findings.iter().map(|f| f.id).collect::<Vec<_>>(),
            [ids::PATCH_UPSTREAM_MOVED]
        );

        // A state that cannot be read is a skipped check, not a note.
        let (findings, skipped) = provenance_findings("x", &side(&[]), &new, &FakeForge::default());
        assert!(findings.is_empty());
        assert_eq!(skipped.len(), 1);
    }
}
