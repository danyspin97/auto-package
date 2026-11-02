//! The submit gate: whether a package whose build status is a failure may be
//! submitted, decided from its results. Pure: the reads are the caller's.

use super::{ArchState, blocks_submit};

/// One result of the package, with the state of its baseline: the same
/// `(repo, arch)` in the origin project, or in the port project for a port
/// repository. `baseline` is `None` when it was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    pub repo: String,
    pub arch: String,
    pub state: ArchState,
    pub baseline: Option<Baseline>,
}

/// The state of a result in the project it is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub project: String,
    pub state: ArchState,
}

/// What the gate decides for a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// The submit may go ahead. `notes` say what was not counted against it.
    Pass { notes: Vec<String> },
    /// The submit is refused, with the reason.
    Refuse(String),
}

/// Decide the gate for `pkg`, whose overall build status is a failure. `primary`
/// is the `(repo, arch)` the package was built for: it must have succeeded.
/// A result still pending refuses (the gate does not wait). A failure its
/// baseline has too is not counted; any other failure refuses unless `allow`
/// (`--allow-arch-failures`). A status that no result explains refuses.
pub fn gate_verdict(
    pkg: &str,
    primary: (&str, &str),
    rows: &[GateRow],
    allow: bool,
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
    let baseline_fails = |r: &GateRow| r.baseline.as_ref().is_some_and(|b| b.state.is_failure());
    let (blocking, shared): (Vec<&GateRow>, Vec<&GateRow>) = failing
        .iter()
        .copied()
        .partition(|r| blocks_submit(&r.state, baseline_fails(r)));
    if failing.is_empty() {
        return GateVerdict::Refuse(format!(
            "its build status is a failure, but no result of {pkg} fails"
        ));
    }
    let mut notes: Vec<String> = shared
        .iter()
        .map(|r| {
            let project = r.baseline.as_ref().map_or("", |b| b.project.as_str());
            format!(
                "{pkg} also fails in {project} on {}: not counted against the submit",
                name(r)
            )
        })
        .collect();
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
