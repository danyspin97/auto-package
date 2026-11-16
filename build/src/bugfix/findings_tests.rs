use super::*;
use crate::request_review::{Finding, Origin, Severity, ids};

fn finding(id: &str, severity: Severity, origin: Origin) -> ReviewFinding {
    ReviewFinding {
        id: id.to_string(),
        severity,
        origin,
        package: "p".into(),
        file: None,
        line: None,
        message: "m".into(),
    }
}

fn routed(id: &str) -> FindingRoute {
    route(&finding(id, Severity::Warning, Origin::Check))
}

fn kind(r: &FindingRoute) -> &'static str {
    match r {
        FindingRoute::Tool(_) => "tool",
        FindingRoute::Agent => "agent",
        FindingRoute::Pipeline => "pipeline",
        FindingRoute::Wait => "wait",
        FindingRoute::Maintainer(_) => "maintainer",
        FindingRoute::NotActedOn => "not acted on",
    }
}

#[test]
fn every_id_of_the_review_is_routed_as_listed() {
    // Every id of the review, once, with where it goes.
    let table: &[(&str, &str)] = &[
        ("request.final-state", "maintainer"),
        ("request.action-not-reviewed", "maintainer"),
        ("request.no-source-rev", "maintainer"),
        ("request.action-malformed", "maintainer"),
        ("request.competing", "maintainer"),
        ("review.incomplete", "maintainer"),
        ("spec.unparsable", "pipeline"),
        ("spec.unfinished", "agent"),
        ("spec.version-macro-unexpanded", "agent"),
        ("spec.style", "agent"),
        ("spec.hygiene", "agent"),
        ("spec.no-check", "agent"),
        ("spec.dropped-tests", "agent"),
        ("changes.missing-entry", "agent"),
        ("changes.old-entries-edited", "tool"),
        ("changes.header-format", "tool"),
        ("changes.date-order", "maintainer"),
        ("changes.version-mismatch", "tool"),
        ("changes.update-without-version-change", "agent"),
        ("changes.no-upstream-notes", "tool"),
        ("changes.dependency-bullet", "tool"),
        ("changes.refresh-not-grouped", "tool"),
        ("changes.patch-not-named", "tool"),
        ("changes.forbidden-text", "agent"),
        ("patch.header", "agent"),
        ("patch.comment", "agent"),
        ("patch.version-in-name", "agent"),
        ("patch.not-source-only", "agent"),
        ("patch.spec-route", "agent"),
        ("patch.unreadable", "maintainer"),
        ("patch.dropped", "maintainer"),
        ("patch.provenance-missing", "maintainer"),
        ("patch.upstream-merged", "maintainer"),
        ("patch.upstream-released", "maintainer"),
        ("patch.upstream-moved", "maintainer"),
        ("patch.upstream-closed", "maintainer"),
        ("rpmlintrc.non-filter", "agent"),
        ("rpmlintrc.set-badness", "agent"),
        ("rpmlintrc.no-reason", "agent"),
        ("risk.new_scriptlet", "maintainer"),
        ("risk.changed_scriptlet", "maintainer"),
        ("risk.new_source_host", "maintainer"),
        ("risk.new_service", "maintainer"),
        ("risk.shell_expansion", "maintainer"),
        ("risk.script_dependency", "maintainer"),
        ("risk.patch_outside_set", "maintainer"),
        ("risk.unanalysed", "maintainer"),
        ("risk.large_patch", "maintainer"),
        ("risk.large_jump", "maintainer"),
        ("source.missing", "pipeline"),
        ("source.unexpanded", "maintainer"),
        ("source.old-archive-kept", "tool"),
        ("files.leftover", "tool"),
        ("files.unreferenced", "agent"),
        ("source.signature", "maintainer"),
        ("source.signature-not-checked", "maintainer"),
        ("source.keyring-changed", "maintainer"),
        ("build.failed", "pipeline"),
        ("build.fails-in-target-too", "maintainer"),
        ("build.no-target-result", "maintainer"),
        ("build.pending", "wait"),
        ("build.no-results", "pipeline"),
        ("build.stale", "pipeline"),
        ("build.disabled-arch", "maintainer"),
        ("build.not-testable", "maintainer"),
        ("version.behind-upstream", "maintainer"),
        ("version.not-checked", "maintainer"),
        ("version.repology-newer", "maintainer"),
        ("agent.finding", "not acted on"),
        ("agent.dispute", "not acted on"),
        ("agent.bad-answer", "not acted on"),
    ];
    let listed: Vec<&str> = table.iter().map(|(id, _)| *id).collect();
    assert_eq!(listed, ids::ALL, "the table lists every id once, in order");
    for (id, want) in table {
        let r = route(&finding(id, Severity::Warning, Origin::Check));
        // An agent id is not acted on whatever its origin.
        assert_eq!(kind(&r), *want, "{id}");
        // No id of the review falls to the table's catch-all.
        assert_ne!(r, FindingRoute::Maintainer(UNKNOWN_ID), "{id}");
    }
}

#[test]
fn an_unknown_id_goes_to_a_person() {
    assert_eq!(
        routed("brand.new-check"),
        FindingRoute::Maintainer(UNKNOWN_ID)
    );
    assert_eq!(routed(""), FindingRoute::Maintainer(UNKNOWN_ID));
}

#[test]
fn a_note_and_an_agent_finding_are_never_acted_on() {
    for id in [
        "changes.dependency-bullet",
        "files.leftover",
        "build.failed",
    ] {
        let note = finding(id, Severity::Note, Origin::Check);
        assert_eq!(route(&note), FindingRoute::NotActedOn, "{id}");
        let opinion = finding(id, Severity::Blocker, Origin::Agent);
        assert_eq!(route(&opinion), FindingRoute::NotActedOn, "{id}");
    }
}

#[test]
fn a_tool_fix_that_refuses_falls_back_to_the_agent_or_a_person() {
    assert_eq!(MechanicalFix::PatchBullet.fallback(), FindingRoute::Agent);
    assert_eq!(MechanicalFix::UpdateBullet.fallback(), FindingRoute::Agent);
    assert_eq!(MechanicalFix::UpstreamNotes.fallback(), FindingRoute::Agent);
}

#[test]
fn a_finding_of_the_review_keeps_every_field() {
    let f =
        Finding::check(ids::FILES_LEFTOVER, Severity::Warning, "p", "m").in_file("a.orig", Some(2));
    let r = ReviewFinding::from(&f);
    assert_eq!(
        (
            r.id.as_str(),
            r.severity,
            r.origin,
            r.file.as_deref(),
            r.line
        ),
        (
            "files.leftover",
            Severity::Warning,
            Origin::Check,
            Some("a.orig"),
            Some(2)
        )
    );
}
