//! The risk flags of the request, from `risk_flags::from_change` over the two
//! sides: the same flags, with the same text, as the submit summary shows.

use std::collections::BTreeMap;

use super::gather::Side;
use super::{Finding, Severity, ids};
use crate::risk_flags::{Files, RiskFlag, from_change, patch_names};

/// What `from_change` reads of one side: the spec, the `_service` file and the
/// patches the spec names.
fn files_of(side: &Side) -> Files {
    let spec = side.spec().map(str::to_string);
    let named = spec.as_deref().map(patch_names).unwrap_or_default();
    let patches: BTreeMap<String, String> = side
        .texts
        .iter()
        .filter(|(n, _)| named.contains(n))
        .map(|(n, t)| (n.clone(), t.clone()))
        .collect();
    Files {
        spec,
        service: side.service().map(str::to_string),
        patches,
    }
}

/// The id of a flag `from_change` produces. The two it does not produce are
/// `None`: `NoUpstreamNotes` is read from the `.changes` entry, and
/// `ReviewerRewrote` comes from a pipeline run.
fn id_of(flag: &RiskFlag) -> Option<&'static str> {
    Some(match flag {
        RiskFlag::NewScriptlet { .. } => ids::RISK_NEW_SCRIPTLET,
        RiskFlag::NewSourceHost { .. } => ids::RISK_NEW_SOURCE_HOST,
        RiskFlag::ShellExpansion { .. } => ids::RISK_SHELL_EXPANSION,
        RiskFlag::ScriptDependency { .. } => ids::RISK_SCRIPT_DEPENDENCY,
        RiskFlag::PatchOutsideSet { .. } => ids::RISK_PATCH_OUTSIDE_SET,
        RiskFlag::NewService { .. } => ids::RISK_NEW_SERVICE,
        RiskFlag::ChangedScriptlet { .. } => ids::RISK_CHANGED_SCRIPTLET,
        RiskFlag::Unanalysed { .. } => ids::RISK_UNANALYSED,
        RiskFlag::LargePatch { .. } => ids::RISK_LARGE_PATCH,
        RiskFlag::LargeJump { .. } => ids::RISK_LARGE_JUMP,
        RiskFlag::NoUpstreamNotes { .. } | RiskFlag::ReviewerRewrote { .. } => return None,
    })
}

/// The risk findings. `archive` lists the files of the new source archive;
/// without it (not downloaded, too large) a patch path cannot be checked and
/// `PatchOutsideSet` is left out, with a note that gives `why` there is none.
pub fn risk_findings(
    package: &str,
    base: &Side,
    new: &Side,
    archive: Option<&[String]>,
    why: &str,
) -> Vec<Finding> {
    let flags = from_change(&files_of(base), &files_of(new), archive.unwrap_or(&[]));
    let mut out = Vec::new();
    let mut unchecked = false;
    for flag in &flags {
        if archive.is_none() && matches!(flag, RiskFlag::PatchOutsideSet { .. }) {
            unchecked = true;
            continue;
        }
        let Some(id) = id_of(flag) else {
            continue;
        };
        let mut f = Finding::check(id, Severity::Warning, package, &flag.line());
        if let RiskFlag::LargePatch { patch, .. } | RiskFlag::PatchOutsideSet { patch, .. } = flag {
            f = f.in_file(patch, None);
        }
        out.push(f);
    }
    if unchecked {
        out.push(Finding::check(
            ids::REVIEW_INCOMPLETE,
            Severity::Note,
            package,
            &format!("patch paths were not checked against the source archive: {why}"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(spec: &str, patches: &[(&str, &str)]) -> Side {
        let mut texts: BTreeMap<String, String> = patches
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        texts.insert("p.spec".into(), spec.into());
        Side {
            listing: Default::default(),
            texts,
            spec_name: Some("p.spec".into()),
        }
    }

    const SPEC: &str = "Name: p\nVersion: 1\nRelease: 0\nSummary: s\nLicense: MIT\n";

    fn big_patch(lines: usize) -> String {
        let mut s = String::from("--- a.c.orig\n+++ a.c\n@@ -1 +1 @@\n");
        for i in 0..lines {
            s.push_str(&format!("+line {i}\n"));
        }
        s
    }

    #[test]
    fn a_large_patch_maps_to_its_id_with_the_summary_text() {
        let spec = format!("{SPEC}Patch0: big.patch\n");
        let base = side(SPEC, &[]);
        let patch = big_patch(300);
        let new = side(&spec, &[("big.patch", &patch)]);
        let found = risk_findings("p", &base, &new, Some(&["a.c".to_string()]), "");
        let f = found
            .iter()
            .find(|f| f.id == ids::RISK_LARGE_PATCH)
            .unwrap();
        let expected = RiskFlag::LargePatch {
            patch: "big.patch".into(),
            lines: 300,
        };
        assert_eq!(f.message, expected.line());
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.file.as_deref(), Some("big.patch"));
    }

    #[test]
    fn without_the_archive_the_outside_set_flag_is_dropped_and_said() {
        let spec = format!("{SPEC}Patch0: p.patch\n");
        let base = side(SPEC, &[]);
        let new = side(
            &spec,
            &[(
                "p.patch",
                "--- x/new.c.orig\n+++ x/new.c\n@@ -1 +1 @@\n-a\n+b\n",
            )],
        );
        let with = risk_findings("p", &base, &new, Some(&["other.c".to_string()]), "");
        assert!(
            with.iter().any(|f| f.id == ids::RISK_PATCH_OUTSIDE_SET),
            "{with:?}"
        );
        let without = risk_findings("p", &base, &new, None, "it was not downloaded");
        assert!(!without.iter().any(|f| f.id == ids::RISK_PATCH_OUTSIDE_SET));
        let note = without
            .iter()
            .find(|f| f.id == ids::REVIEW_INCOMPLETE)
            .unwrap();
        assert_eq!(note.severity, Severity::Note);
        assert!(note.message.ends_with("it was not downloaded"));
        let listed = risk_findings("p", &base, &new, None, "its file list could not be read");
        assert!(
            listed
                .iter()
                .any(|f| f.message.ends_with("could not be read"))
        );
    }

    #[test]
    fn a_new_scriptlet_is_flagged() {
        let base = side(SPEC, &[]);
        let new = side(&format!("{SPEC}\n%post\necho hi\n"), &[]);
        let found = risk_findings("p", &base, &new, None, "it was not downloaded");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, ids::RISK_NEW_SCRIPTLET);
    }

    #[test]
    fn an_unchanged_spec_has_no_flag() {
        let s = side(SPEC, &[]);
        assert!(risk_findings("p", &s, &s, None, "").is_empty());
    }
}
