//! The findings `lint` reports for the provenance of carried patches (plan
//! 4.2): what tidy writes, what the pipeline acts on, and what is only reported.

use super::{Provenance, Verdict};
use crate::quality::{Finding, Kind};

/// Whether tidy has a header or a spec comment to write for `p`.
pub fn header_due(p: &Provenance) -> bool {
    (p.header.is_some() && !p.has_upstream_line) || p.spec_comment_due
}

/// One finding per thing to report or act on, for each patch.
pub fn lint_findings(provenance: &[Provenance]) -> Vec<Finding> {
    provenance.iter().flat_map(findings_of).collect()
}

fn findings_of(p: &Provenance) -> Vec<Finding> {
    let mut out = Vec::new();
    let url = p.origin.as_ref().map(|o| o.url.as_str()).unwrap_or("");
    if header_due(p) {
        out.push(finding(
            Kind::PatchProvenance,
            format!(
                "{}: the header or spec comment lacks the upstream request {url}",
                p.name
            ),
        ));
    }
    let (kind, detail) = match &p.verdict {
        Verdict::Nothing => return out,
        Verdict::Redundant { tag } => (
            Kind::PatchUpstreamMerged,
            format!(
                "{}: {url} is merged and in {tag}, which the package ships",
                p.name
            ),
        ),
        Verdict::ReleasedLater { tag, version } => (
            Kind::PatchUpstreamReleased,
            format!(
                "{}: {url} is merged in {tag}, after the packaged release; update to {version} takes it",
                p.name
            ),
        ),
        Verdict::HeadMoved { head, .. } => (
            Kind::PatchUpstreamMoved,
            format!(
                "{}: {url} moved to {} with different code",
                p.name,
                &head[..head.len().min(12)]
            ),
        ),
        Verdict::MovedNotComparable(why) => (
            Kind::PatchUpstreamNotChecked,
            format!("{}: {url} moved and cannot be compared: {why}", p.name),
        ),
        Verdict::Closed => (
            Kind::PatchUpstreamClosed,
            format!(
                "{}: {url} is closed without a merge; a maintainer decides whether the patch stays",
                p.name
            ),
        ),
        Verdict::NotChecked(why) => (
            Kind::PatchUpstreamNotChecked,
            format!("{}: upstream state not checked: {why}", p.name),
        ),
    };
    out.push(finding(kind, detail));
    out
}

fn finding(kind: Kind, detail: String) -> Finding {
    Finding { kind, detail }
}
