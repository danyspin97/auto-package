use super::*;
use crate::quality::{Kind, needs_pipeline};

fn prov(verdict: Verdict) -> Provenance {
    Provenance {
        name: "106.patch".into(),
        origin: None,
        has_upstream_line: false,
        status: Err("x".into()),
        containment: None,
        verdict,
        header: None,
        spec_comment_due: false,
    }
}

fn kinds(p: Provenance) -> Vec<Kind> {
    lint_findings(&[p]).iter().map(|f| f.kind).collect()
}

#[test]
fn each_verdict_has_its_kind() {
    assert_eq!(kinds(prov(Verdict::Nothing)), Vec::<Kind>::new());
    assert_eq!(
        kinds(prov(Verdict::Redundant { tag: "v1".into() })),
        [Kind::PatchUpstreamMerged]
    );
    assert_eq!(
        kinds(prov(Verdict::ReleasedLater {
            tag: "v2".into(),
            version: "2".into()
        })),
        [Kind::PatchUpstreamReleased]
    );
    assert_eq!(
        kinds(prov(Verdict::HeadMoved {
            head: "a".repeat(40),
            patch_url: "u".into()
        })),
        [Kind::PatchUpstreamMoved]
    );
    assert_eq!(
        kinds(prov(Verdict::MovedNotComparable("why".into()))),
        [Kind::PatchUpstreamNotChecked]
    );
    assert_eq!(kinds(prov(Verdict::Closed)), [Kind::PatchUpstreamClosed]);
    assert_eq!(
        kinds(prov(Verdict::NotChecked("rate limit".into()))),
        [Kind::PatchUpstreamNotChecked]
    );
}

#[test]
fn a_missing_header_is_a_mechanical_finding() {
    let mut p = prov(Verdict::Nothing);
    p.spec_comment_due = true;
    assert!(header_due(&p));
    assert_eq!(kinds(p), [Kind::PatchProvenance]);
}

#[test]
fn the_kinds_split_into_pipeline_mechanical_and_report_only() {
    let pipeline = [Kind::PatchUpstreamMerged];
    let report = [
        Kind::PatchUpstreamMoved,
        Kind::PatchUpstreamReleased,
        Kind::PatchUpstreamClosed,
        Kind::PatchUpstreamNotChecked,
    ];
    for k in pipeline {
        assert!(!k.is_mechanical() && !k.is_report_only(), "{k:?}");
    }
    for k in report {
        assert!(k.is_report_only() && !k.is_mechanical(), "{k:?}");
    }
    assert!(Kind::PatchProvenance.is_mechanical());
    assert!(!Kind::PatchProvenance.is_report_only());
    assert!(needs_pipeline(&lint_findings(&[prov(
        Verdict::Redundant { tag: "v".into() }
    )])));
    assert!(!needs_pipeline(&lint_findings(&[prov(Verdict::Closed)])));
    assert_eq!(Kind::PatchUpstreamMoved.label(), "patch-upstream-moved");
}
