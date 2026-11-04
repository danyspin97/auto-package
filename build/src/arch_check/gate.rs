//! The submit gate: whether a package whose build status is a failure may be
//! submitted, decided from its results. Pure: the reads are the caller's.

use super::{ArchState, BaselineState, blocks_submit};

/// One result of the package. A failure carries its baseline: the same
/// `(repo, arch)` in the origin project, or in the port project for a port
/// repository. `baseline` is `None` for a result that is not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    pub repo: String,
    pub arch: String,
    pub state: ArchState,
    pub baseline: Option<Baseline>,
}

/// The state of a failure in the project it is compared with; `state` is
/// `None` when that project has no result of the package for the arch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub project: String,
    pub state: Option<ArchState>,
}

/// What the gate decides for a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// The submit may go ahead. `notes` say what was not counted against it.
    Pass { notes: Vec<String> },
    /// The submit is refused, with the reason.
    Refuse(String),
}

/// Decide the gate for `pkg`. `primary` is the `(repo, arch)` the package was
/// built for: it must have succeeded. A result still pending refuses (the gate
/// does not wait). A failure its baseline fails too, or has no result for, is
/// not counted; any other failure refuses unless `allow`
/// (`--allow-arch-failures`). When the overall
/// build status is a failure (`overall_failed`), a status no result explains
/// refuses. Port results that are not testable or not final do not block.
pub fn gate_verdict(
    pkg: &str,
    primary: (&str, &str),
    rows: &[GateRow],
    allow: bool,
    overall_failed: bool,
) -> GateVerdict {
    let name = |r: &GateRow| format!("{}/{}", r.repo, r.arch);
    let is_primary = |r: &GateRow| (r.repo.as_str(), r.arch.as_str()) == primary;
    match rows.iter().find(|r| is_primary(r)) {
        None => {
            return GateVerdict::Refuse(format!(
                "{pkg} has no result on {}/{}",
                primary.0, primary.1
            ));
        }
        Some(r) if r.state != ArchState::Succeeded => {
            return GateVerdict::Refuse(format!(
                "it does not build on {} ({})",
                name(r),
                r.state.label()
            ));
        }
        Some(_) => {}
    }
    let others: Vec<&GateRow> = rows.iter().filter(|r| !is_primary(r)).collect();
    let pending: Vec<String> = others
        .iter()
        .filter(|r| r.state.is_pending())
        .map(|r| format!("{} ({})", name(r), r.state.label()))
        .collect();
    if !pending.is_empty() {
        return GateVerdict::Refuse(format!(
            "it is still building on {}: its results are not final",
            pending.join(", ")
        ));
    }
    let failing: Vec<&GateRow> = others
        .iter()
        .filter(|r| r.state.is_failure())
        .copied()
        .collect();
    // A failure without a baseline was not read: it blocks, as an unread
    // baseline does in the check.
    let baseline_of = |r: &GateRow| {
        r.baseline.as_ref().map_or(BaselineState::Builds, |b| {
            BaselineState::of(b.state.as_ref())
        })
    };
    let (blocking, shared): (Vec<&GateRow>, Vec<&GateRow>) = failing
        .iter()
        .copied()
        .partition(|r| blocks_submit(&r.state, baseline_of(r)));
    if overall_failed && failing.is_empty() {
        return GateVerdict::Refuse(format!(
            "its build status is a failure, but no result of {pkg} fails"
        ));
    }
    // Nothing was built on another architecture: the pass below says nothing
    // about them, so the notes say so.
    let mut notes: Vec<String> = Vec::new();
    if !others.is_empty() && others.iter().all(|r| r.state.is_skipped()) {
        notes.push(format!(
            "{pkg} is not checked on the other architectures: every result there is disabled \
             or excluded in the branch"
        ));
    }
    notes.extend(shared.iter().map(|r| {
        let project = r.baseline.as_ref().map_or("", |b| b.project.as_str());
        match baseline_of(r) {
            BaselineState::NoResult => format!(
                "{pkg} fails on {}, and {project} has no result there: not counted against the submit",
                name(r)
            ),
            _ => format!(
                "{pkg} also fails in {project} on {}: not counted against the submit",
                name(r)
            ),
        }
    }));
    if blocking.is_empty() {
        return GateVerdict::Pass { notes };
    }
    let names: Vec<String> = blocking
        .iter()
        .map(|r| format!("{} ({})", name(r), r.state.label()))
        .collect();
    if !allow {
        return GateVerdict::Refuse(format!(
            "it fails on {}; pass --allow-arch-failures to submit anyway",
            names.join(", ")
        ));
    }
    notes.push(format!(
        "{pkg} fails on {}: submitted anyway (--allow-arch-failures)",
        names.join(", ")
    ));
    GateVerdict::Pass { notes }
}
