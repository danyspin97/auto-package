//! `fix --request ID`: resolve what a review found wrong in a submit request,
//! in the request's branch project. The findings are routed (see
//! `ap_build::bugfix::route`), the run fixes them with the update machinery,
//! the checks run again on the branch and the delta is printed. With
//! `--submit` a new request supersedes the old one, after the usual gates and
//! a confirmation. The target project is only read.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use ap_build::bugfix::{FindingRoute, FindingsJob, FixTask, ReviewFinding, route};
use ap_build::obs_status::ResultEntry;
use ap_build::osc::{self, DiffSpec, OpenRequest};
use ap_build::proc::Emit;
use ap_build::request_review::fetch::{LiveRequestObs, ReadFailure, RequestObs};
use ap_build::request_review::gather::{Inputs, gather};
use ap_build::request_review::render::{render_json, render_text};
use ap_build::request_review::request::{ActionSource, ActionTarget};
use ap_build::request_review::run::{Deps, Options, Reviewed, review};
use ap_build::request_review::save::{review_dir, save};
use ap_build::request_review::upstream_check::{live_plan, live_repology};
use ap_build::workspace::Workspace;
use ap_core::sink::{HumanSink, LogLevel};
use color_eyre::eyre::eyre;

use super::findings::*;
use super::plan::*;
use super::resubmit::{SubmitCtx, submit};
use super::{FixArgs, FixExit, state_root, update_args};

/// The exit status of a refused fix: the message, as `FixExit`.
pub(super) fn exit<T>(code: i32, message: impl Into<String>) -> color_eyre::Result<T> {
    Err(FixExit {
        code,
        message: message.into(),
    }
    .into())
}

/// The findings of a review as the fix sees them, with each one's route.
fn routed(findings: &[ReviewFinding]) -> Vec<(ReviewFinding, FindingRoute)> {
    findings.iter().map(|f| (f.clone(), route(f))).collect()
}

/// The lines that say what each finding is routed to (the dry run and the
/// summary of a run share them).
fn route_lines(
    id: &str,
    source: &ActionSource,
    target: &ActionTarget,
    routed: &[(ReviewFinding, FindingRoute)],
) -> Vec<String> {
    let names = |want: &dyn Fn(&FindingRoute) -> bool| -> String {
        let items: Vec<String> = routed
            .iter()
            .filter(|(_, r)| want(r))
            .map(|(f, _)| match &f.file {
                Some(file) => format!("{} ({file})", f.id),
                None => f.id.clone(),
            })
            .collect();
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    let maintainer = routed
        .iter()
        .filter(|(f, r)| {
            matches!(r, FindingRoute::Maintainer(_))
                && f.severity != ap_build::request_review::Severity::Note
        })
        .map(|(f, r)| match r {
            FindingRoute::Maintainer(why) => format!("{} ({why})", f.id),
            _ => unreachable!("filtered to maintainer routes"),
        })
        .collect::<Vec<_>>();
    vec![
        format!(
            "request {id}: {}/{}@{} -> {}/{}",
            source.project,
            source.package,
            source.rev.as_deref().unwrap_or("(no revision pinned)"),
            target.project,
            target.package
        ),
        format!(
            "  fixed by the tool:   {}",
            names(&|r| matches!(r, FindingRoute::Tool(_)))
        ),
        format!(
            "  fixed by the agent:  {}",
            names(&|r| matches!(r, FindingRoute::Agent))
        ),
        format!(
            "  fixed by the build:  {}",
            names(&|r| matches!(r, FindingRoute::Pipeline))
        ),
        format!(
            "  waits for OBS:       {}",
            names(&|r| matches!(r, FindingRoute::Wait))
        ),
        format!(
            "  needs a maintainer:  {}",
            if maintainer.is_empty() {
                "none".to_string()
            } else {
                maintainer.join(", ")
            }
        ),
        format!(
            "  not acted on:        {}",
            names(&|r| matches!(r, FindingRoute::NotActedOn))
        ),
    ]
}

/// The version in a spec, if it has one.
fn version_of(spec: Option<&str>) -> Option<String> {
    ap_build::spec::parse_header(spec?).map(|h| h.version)
}

/// Read the request's findings: from a saved review, or by a review run now
/// (which is saved under `<work-dir>/reviews/<id>/<run-id>/`).
fn review_now(
    obs: &dyn RequestObs,
    id: &str,
    work_dir: &Path,
    level: LogLevel,
) -> color_eyre::Result<(Reviewed, PathBuf)> {
    let sink = HumanSink::new(level);
    let run_id = ap_core::run::new_run_id();
    let dir = review_dir(work_dir, id, &run_id.0);
    std::fs::create_dir_all(&dir).map_err(|e| eyre!("cannot create {}: {e}", dir.display()))?;
    let scratch = dir.join("sources");
    let reviewed = run_checks(obs, id, &scratch, &sink);
    // The downloads are the review's scratch, not run data.
    let _ = std::fs::remove_dir_all(&scratch);
    let reviewed = match reviewed {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(eyre!(e));
        }
    };
    let text = render_text(&reviewed.report);
    let json = render_json(&reviewed.report);
    save(&dir, &reviewed, &text, &json).map_err(|e| eyre!("cannot save the review: {e}"))?;
    Ok((reviewed, dir))
}

/// The deterministic checks of request `id`, with the live lookups (the same
/// as `review` without `--agent`).
fn run_checks(
    obs: &dyn RequestObs,
    id: &str,
    scratch: &Path,
    sink: &HumanSink,
) -> Result<Reviewed, String> {
    let plan = live_plan(Emit::run(sink));
    let hosts = ap_build::allowed_hosts::load_default();
    let host_allowed = |host: &str| hosts.allows(host, ap_build::allowed_hosts::Use::Source);
    let deps = Deps {
        plan: &plan,
        repology: &live_repology,
        ldconfig_macro: &crate::review_cmd::ldconfig_macro,
        host_allowed: &host_allowed,
    };
    let options = Options {
        id,
        download: true,
        scratch,
        fail_on: ap_build::request_review::Severity::Warning,
        fail_on_agent: false,
        build: env!("AP_BUILD_REVISION"),
    };
    review(obs, &options, &deps).map_err(|e| e.to_string())
}

/// The request as it is after the fix: the same request with its source
/// pinned to `rev`. Every other read is the live one.
struct Pinned<'a> {
    live: &'a dyn RequestObs,
    xml: String,
}

impl RequestObs for Pinned<'_> {
    fn request(&self, _id: &str) -> Result<String, ReadFailure> {
        Ok(self.xml.clone())
    }
    fn listing(
        &self,
        project: &str,
        pkg: &str,
        rev: Option<&str>,
    ) -> Result<Option<ap_build::osc::Listing>, ReadFailure> {
        self.live.listing(project, pkg, rev)
    }
    fn file(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: Option<&str>,
    ) -> Result<String, ReadFailure> {
        self.live.file(project, pkg, file, rev)
    }
    fn download(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: &str,
        into: &Path,
    ) -> Result<PathBuf, ReadFailure> {
        self.live.download(project, pkg, file, rev, into)
    }
    fn diff(&self, spec: &DiffSpec<'_>) -> Result<String, ReadFailure> {
        self.live.diff(spec)
    }
    fn results(&self, project: &str, pkg: &str) -> Result<Option<Vec<ResultEntry>>, String> {
        self.live.results(project, pkg)
    }
    fn open_requests_to(&self, project: &str, pkg: &str) -> Result<Vec<OpenRequest>, ReadFailure> {
        self.live.open_requests_to(project, pkg)
    }
    fn current_srcmd5(&self, project: &str, pkg: &str) -> Result<String, ReadFailure> {
        self.live.current_srcmd5(project, pkg)
    }
}

/// A request document with one submit action from `source` (pinned to `rev`)
/// to `target`, in the state and by the creator of `doc`.
pub(super) fn pinned_xml(
    doc: &ap_build::request_review::request::RequestDoc,
    source: &ActionSource,
    target: &ActionTarget,
    rev: &str,
) -> String {
    use ap_build::obs_xml::escape;
    format!(
        "<request id=\"{}\" creator=\"{}\"><action type=\"submit\"><source project=\"{}\" package=\"{}\" rev=\"{}\"/><target project=\"{}\" package=\"{}\"/></action><state name=\"{}\" who=\"{}\" when=\"{}\" created=\"{}\"/></request>",
        escape(&doc.id),
        escape(&doc.creator),
        escape(&source.project),
        escape(&source.package),
        escape(rev),
        escape(&target.project),
        escape(&target.package),
        escape(&doc.state.name),
        escape(&doc.state.who),
        escape(&doc.state.when),
        escape(&doc.state.created),
    )
}

/// A run id that is unique enough for one fix: the clock and the process.
fn new_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{}", std::process::id())
}

/// `fix --request ID`.
pub(super) fn run(a: &FixArgs, id: &str, summary: &mut Option<String>) -> color_eyre::Result<()> {
    let level: LogLevel = a.log.parse().map_err(|e| eyre!("{e}"))?;
    let sink = HumanSink::new(level);
    let emit = Emit::run(&sink);
    let obs = LiveRequestObs { emit };
    // The same terminal check as `update --submit`, before any read from OBS: nothing is
    // built or sent when no one can confirm.
    let is_tty = std::io::stdin().is_terminal();
    if let Some(message) =
        crate::update_project::needs_terminal(a.submit, false, a.yes, a.dry_run, is_tty)
    {
        return Err(crate::session::NotSubmitted(message).into());
    }
    let user = osc::current_user(emit).map_err(|e| eyre!("{e}"))?;
    let work_dir = std::path::absolute(&a.work_dir)?;

    // A request this run may not fix is refused before anything is downloaded or written.
    let early = gather(&obs, id).map_err(|e| eyre!("{}", e.0))?;
    request_refusal(&early.doc, &user).map_err(color_eyre::Report::from)?;

    // The findings: a saved review (checked against the request below) or a review now.
    let (inputs, before, saved): (Inputs, Vec<ReviewFinding>, Option<Saved>) = match &a.findings {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| eyre!("cannot read {}: {e}", path.display()))?;
            let saved = parse_saved(&text, id).map_err(|e| eyre!(e))?;
            let findings = saved.findings.clone();
            (early, findings, Some(saved))
        }
        None => {
            let (reviewed, _dir) = review_now(&obs, id, &work_dir, level)?;
            let findings = reviewed
                .report
                .check_findings()
                .map(ReviewFinding::from)
                .collect();
            (reviewed.inputs, findings, None)
        }
    };
    let (source, target) = request_refusal(&inputs.doc, &user).map_err(color_eyre::Report::from)?;
    let (branch, pkg, origin) = (
        source.project.clone(),
        target.package.clone(),
        target.project.clone(),
    );
    let (sp, spkg) = (source.project.as_str(), source.package.as_str());

    // The source must be the one the review read, and the branch must still be at it.
    if let Some(saved) = &saved
        && let Some(why) = saved_mismatch(saved, &source)
    {
        return exit(EXIT_NEEDS_MAINTAINER, why);
    }
    match &source.rev {
        None if !a.submit => {
            return exit(
                EXIT_NEEDS_MAINTAINER,
                format!(
                    "request {id} follows its source (no revision pinned): run with --submit, which supersedes it with a pinned one"
                ),
            );
        }
        Some(rev) => {
            let current = obs.current_srcmd5(sp, spkg).map_err(|e| eyre!("{e}"))?;
            let at_rev = obs
                .listing(sp, spkg, Some(rev))
                .map_err(|e| eyre!("{e}"))?
                .and_then(|l| l.srcmd5);
            if at_rev.as_deref() != Some(current.as_str()) {
                return exit(
                    EXIT_NEEDS_MAINTAINER,
                    format!("{branch}/{pkg} moved since request {id} was made: review it again"),
                );
            }
        }
        None => {}
    }

    let [action] = inputs.actions.as_slice() else {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!("request {id} has no single submit action"),
        );
    };
    if action.target_missing {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!("{origin}/{pkg} does not exist: a new package is not fixed here"),
        );
    }
    let (branch_version, target_version) =
        (version_of(action.new.spec()), version_of(action.old.spec()));
    let (Some(branch_version), Some(target_version)) = (branch_version, target_version) else {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!("the spec of {pkg} has no Version: the findings cannot be fixed"),
        );
    };
    if branch_version != target_version {
        return exit(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {id} changes the version ({target_version} to {branch_version}): a fix keeps the version; update the package instead"
            ),
        );
    }

    let routed = routed(&before);
    let lines = route_lines(id, &source, &target, &routed);
    for line in &lines {
        println!("{line}");
    }
    // Refused before anything is written: the agent is needed and not allowed,
    // or no finding is for the tool.
    let agent_ids: Vec<&str> = routed
        .iter()
        .filter(|(_, r)| matches!(r, FindingRoute::Agent))
        .map(|(f, _)| f.id.as_str())
        .collect();
    let refusal = if !agent_ids.is_empty() && a.no_agent {
        Some(FixExit {
            code: EXIT_NEEDS_MAINTAINER,
            message: format!(
                "--no-agent: the agent is needed for {}",
                agent_ids.join(", ")
            ),
        })
    } else if !routed.iter().any(|(_, r)| is_actionable(r)) {
        let blocked = routed.iter().any(|(f, r)| {
            matches!(r, FindingRoute::Maintainer(_))
                && f.severity == ap_build::request_review::Severity::Blocker
        });
        Some(FixExit {
            code: if blocked {
                EXIT_NEEDS_MAINTAINER
            } else {
                EXIT_NO_FIX
            },
            message: "no finding the tool may fix".to_string(),
        })
    } else {
        None
    };
    if a.dry_run {
        println!(
            "dry run: nothing is branched, built, committed or submitted; {origin}/{pkg} stays untouched"
        );
        println!(
            "would: rebuild {branch}/{pkg} with the fixes, check the request again{}",
            if a.submit {
                format!("; with --submit: create a new request superseding {id} after confirmation")
            } else {
                String::new()
            }
        );
        return refusal.map_or(Ok(()), |f| Err(f.into()));
    }
    if let Some(f) = refusal {
        return Err(f.into());
    }

    // The fix: the update run with the findings as its task, in the branch.
    let state_root = state_root(&a.work_dir, &branch, &a.arch);
    let nonce = new_nonce();
    let job = FindingsJob {
        request_id: id.to_string(),
        source_project: branch.clone(),
        target_project: origin.clone(),
        pkg: pkg.clone(),
        version: branch_version.clone(),
        findings: routed,
        target_changes: action.old.changes().map(str::to_string),
        target_files: action.old.file_names(),
        nonce: nonce.clone(),
    };
    let mut b = a.clone();
    b.project = Some(origin.clone());
    b.only = vec![pkg.clone()];
    b.branch_project = Some(branch.clone());
    b.submit = false;
    b.yes = false;
    let update = update_args(&b, branch.clone(), Some(FixTask::Findings(job)));
    let result = crate::update_project::run(update, summary);
    let end = Workspace::find_by_obs_name(&state_root, &pkg)
        .and_then(|ws| ap_build::bugfix::read_fix_end(ws.dir(), &nonce));
    match ending(result.is_ok(), end.as_ref()) {
        Ending::Fixed => {}
        Ending::Exit(code, message) => return Err(FixExit { code, message }.into()),
        Ending::Failed => match result {
            Err(e) => return Err(e),
            Ok(()) => return Err(eyre!("the fix ended without a verdict: see the run's log")),
        },
    }

    // The checks again, on the branch as it is now, against the same target.
    let head = obs
        .current_srcmd5(&branch, &pkg)
        .map_err(|e| eyre!("{e}"))?;
    let pinned_source = ActionSource {
        project: branch.clone(),
        package: pkg.clone(),
        rev: Some(head.clone()),
    };
    let xml = pinned_xml(&inputs.doc, &pinned_source, &target, &head);
    let pinned = Pinned { live: &obs, xml };
    let (reviewed, _dir) = review_now(&pinned, id, &work_dir, level)?;
    let after: Vec<ReviewFinding> = reviewed
        .report
        .check_findings()
        .map(ReviewFinding::from)
        .collect();
    let d = delta(&before, &after);
    println!("re-checked {branch}/{pkg} at {head}:");
    for f in &d.resolved {
        println!("  resolved:   {}", f.id);
    }
    for f in &d.introduced {
        println!("  new:        {}", f.id);
    }
    for f in &d.left {
        println!("  left:       {}", f.id);
    }
    let left_refusal = not_submittable(&after);
    if !a.submit {
        println!(
            "nothing was submitted: run with --submit to create a request that supersedes {id}"
        );
        return left_refusal.map_or(Ok(()), |f| Err(f.into()));
    }
    if let Some(f) = left_refusal {
        return Err(f.into());
    }
    submit(
        a,
        SubmitCtx {
            obs: &obs,
            sink: &sink,
            id,
            user: &user,
            creator: &inputs.doc.creator,
            state: &inputs.doc.state.name,
            branch: &branch,
            pkg: &pkg,
            origin: &origin,
            branch_version: &branch_version,
            target_version: &target_version,
            state_root: &state_root,
            resolved: &d.resolved,
            left: &d.left,
            after: &after,
        },
    )
}
