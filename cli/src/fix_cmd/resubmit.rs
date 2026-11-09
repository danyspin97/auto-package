//! The re-submit of `fix --request --submit`: a new request from the branch
//! that supersedes the reviewed one, after the gates and a confirmation. The
//! old request is superseded only after the new one exists.

use std::path::Path;

use ap_build::arch_check::ArchDeadline;
use ap_build::bugfix::ReviewFinding;
use ap_build::osc;
use ap_build::proc::Emit;
use ap_build::request_review::fetch::RequestObs;
use ap_build::request_review::request::parse_request;
use ap_build::update::Ecosystem;
use ap_build::update_project::{self as up, Item, ItemOutcome, SrOutcome};
use ap_core::sink::HumanSink;
use color_eyre::eyre::eyre;

use super::FixArgs;
use super::findings::*;
use super::plan::*;
use super::request::exit;
use crate::update_project::{ArchWait, confirm_with, prepare_request, review_gate, yes_refusal};

/// What the submit step needs from the run.
pub(super) struct SubmitCtx<'a> {
    pub(super) obs: &'a dyn RequestObs,
    pub(super) sink: &'a HumanSink,
    pub(super) id: &'a str,
    pub(super) user: &'a str,
    pub(super) creator: &'a str,
    pub(super) state: &'a str,
    pub(super) branch: &'a str,
    pub(super) pkg: &'a str,
    pub(super) origin: &'a str,
    pub(super) branch_version: &'a str,
    pub(super) target_version: &'a str,
    pub(super) state_root: &'a Path,
    pub(super) resolved: &'a [ReviewFinding],
    pub(super) left: &'a [ReviewFinding],
    pub(super) after: &'a [ReviewFinding],
}

/// The re-submit: a new request from the branch that supersedes `id`, after
/// the gates and a confirmation (`--yes` skips the confirmation and refuses
/// what a person must decide).
pub(super) fn submit(a: &FixArgs, c: SubmitCtx<'_>) -> color_eyre::Result<()> {
    let emit = Emit::run(c.sink);
    // Read again: the request must still be the one that was reviewed.
    let now =
        parse_request(&c.obs.request(c.id).map_err(|e| eyre!("{e}"))?).map_err(|e| eyre!(e))?;
    if now.state.name != c.state || now.creator != c.creator {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {} changed while it was fixed: nothing was submitted",
                c.id
            ),
        );
    }
    if c.user != c.creator {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {} is {}'s: only its creator can supersede it",
                c.id, c.creator
            ),
        );
    }
    if a.yes {
        if let Some(why) = needs_person(c.after) {
            return exit(
                EXIT_NEEDS_MAINTAINER,
                format!("--yes refuses to submit: {why}; run without --yes to review it"),
            );
        }
        let gate = review_gate(c.state_root, &[c.pkg]);
        if let Some(lines) = yes_refusal(true, &gate) {
            return exit(EXIT_NEEDS_MAINTAINER, lines.join("\n"));
        }
    }
    let item = Item {
        pkg: c.pkg.to_string(),
        origin: c.origin.to_string(),
        // The request does not use the ecosystem: the branch is built at its own version.
        ecosystem: Ecosystem::Repology {
            project: c.pkg.to_string(),
        },
        old_version: c.target_version.to_string(),
        new_version: c.branch_version.to_string(),
        upstream_tag: None,
        held_version: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: c
            .resolved
            .iter()
            .map(|f| format!("{} {}", f.id, f.message))
            .collect(),
    };
    // One package, one bounded wait for its other architectures.
    let wait = ArchDeadline::starting_at(osc::OVERALL_MAX_WAIT, std::time::Instant::now());
    let mut sr = match prepare_request(
        c.branch,
        a.distro.as_deref(),
        &a.arch,
        c.state_root,
        &item,
        // A fix run never tidies a header only (that is lint's), so no Skipped
        // item is resubmitted here; a branch with no change is refused inside.
        &ItemOutcome::Updated,
        false,
        // A fix run runs no arch check, so it checked no port (see `gate_counts_ports`).
        false,
        ArchWait::Until(&wait),
        c.sink,
        Some(c.id),
    ) {
        Ok(sr) => sr,
        Err(SrOutcome::Failed(why)) => return Err(eyre!("{why}")),
        Err(SrOutcome::Existing(other)) => {
            return exit(
                EXIT_NEEDS_MAINTAINER,
                format!(
                    "request {other} from {} is already open: nothing was submitted",
                    c.branch
                ),
            );
        }
        Err(SrOutcome::NotEligible(why)) => {
            return exit(EXIT_NEEDS_MAINTAINER, format!("no request: {why}"));
        }
        Err(other) => return exit(EXIT_NEEDS_MAINTAINER, format!("no request: {other:?}")),
    };
    sr.description = findings_description(c.id, c.resolved, c.after);
    for line in up::sr_confirmation_lines(c.branch, &[&item]) {
        println!("  {line}");
    }
    println!(
        "supersedes request {}; resolves {} finding(s); left: {}",
        c.id,
        c.resolved.len(),
        c.left.len()
    );
    if !a.yes
        && !confirm_with(
            &format!("Create a request that supersedes {}?", c.id),
            &|m| eprintln!("{m}"),
        )
    {
        println!("nothing was submitted");
        return Ok(());
    }
    let new_id = create_and_supersede(&LiveResubmit { emit }, &sr, c.id).map_err(|e| eyre!(e))?;
    let _ = ap_build::fix_flags::clear_submitted(c.state_root, c.pkg);
    let _ = ap_build::risk_flags::clear_submitted(c.state_root, c.pkg);
    println!("request {} is superseded by request {new_id}", c.id);
    Ok(())
}

/// The writes of a re-submit, behind a trait so their order is tested without
/// OBS.
pub(super) trait ResubmitObs {
    /// Create the request; its id.
    fn create(&self, sr: &osc::SubmitRequest) -> Result<String, String>;
    /// Mark `old` superseded by `new`, with `comment` as the reason.
    fn supersede(&self, old: &str, new: &str, comment: &str) -> Result<(), String>;
}

/// The writes through `osc`.
pub(super) struct LiveResubmit<'a> {
    pub(super) emit: Emit<'a>,
}

impl ResubmitObs for LiveResubmit<'_> {
    fn create(&self, sr: &osc::SubmitRequest) -> Result<String, String> {
        osc::create_submit_request(sr, self.emit).map_err(|e| e.to_string())
    }
    fn supersede(&self, old: &str, new: &str, comment: &str) -> Result<(), String> {
        osc::supersede_request(old, new, comment, self.emit).map_err(|e| e.to_string())
    }
}

/// Create the new request, then supersede `old` by it. A request that is not
/// created supersedes nothing. When the supersede fails the new request stays
/// open, and the error names both ids.
pub(super) fn create_and_supersede(
    obs: &dyn ResubmitObs,
    sr: &osc::SubmitRequest,
    old: &str,
) -> Result<String, String> {
    let new = obs
        .create(sr)
        .map_err(|e| format!("could not create the request: {e}"))?;
    let comment = format!("Superseded by request {new}: its findings are fixed in the branch");
    obs.supersede(old, &new, &comment).map_err(|e| {
        format!(
            "created request {new}, but request {old} was not superseded: {e}; request {old} is untouched"
        )
    })?;
    Ok(new)
}
