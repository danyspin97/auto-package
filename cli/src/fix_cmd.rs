//! `auto-package fix`: fix one reported bug in a package of a project, in a
//! branch project, and optionally submit it.
//!
//! The bug text and the upstream fix are looked up here, before anything is
//! branched ([`ap_build::bugfix::prepare_fix`]); the run itself is the
//! `update` run in branch mode with the package at the version it has and the
//! fix as its task.

mod args;
mod plan;
#[cfg(test)]
mod tests;

pub use args::FixArgs;

use ap_build::bugfix::{self, FixRequest, FixTask, LiveNet};
use ap_build::proc::Emit;
use clap::Parser;
use color_eyre::eyre::eyre;

use crate::update_project::UpdateProjectArgs;
use plan::*;

/// A fix that ended with a status of its own (3 for no fix to make, 4 for a
/// maintainer): `main` exits with it after its cleanup.
#[derive(Debug)]
pub struct FixExit {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for FixExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FixExit {}

/// The `update` arguments of a fix: parsed from a plain `update --project P`,
/// so every default stays in the clap attributes, then set.
pub(crate) fn update_args(
    a: &FixArgs,
    branch_project: String,
    task: Option<FixTask>,
) -> UpdateProjectArgs {
    let argv = ["auto-package", "update", "--project", a.project.as_str()];
    let mut u = match crate::Cli::try_parse_from(argv).map(|c| c.command) {
        Ok(crate::Commands::Update(u)) => u,
        _ => unreachable!("`update --project P` always parses"),
    };
    u.only = a.only.clone();
    u.branch_project = Some(branch_project);
    u.follow_devel = a.follow_devel;
    u.work_dir = a.work_dir.clone();
    u.notes_dir = a.notes_dir.clone();
    u.distro = a.distro.clone();
    u.arch = a.arch.clone();
    u.budget_usd = a.budget_usd;
    u.max_attempts = a.max_attempts;
    // A dry run asks no model.
    u.no_agent = a.no_agent || a.dry_run;
    u.submit = a.submit;
    u.yes = a.yes;
    u.dry_run = a.dry_run;
    u.tui = a.tui;
    u.log = a.log.clone();
    u.allow_host = a.allow_host.clone();
    u.limits = a.limits.clone();
    u.fix_task = task.map(std::sync::Arc::new);
    u
}

/// `fix`: the whole command.
pub fn run(mut a: FixArgs, summary: &mut Option<String>) -> color_eyre::Result<()> {
    let [pkg] = a.only.as_slice() else {
        return Err(eyre!(
            "fix takes exactly one package: --only PACKAGE (got {})",
            a.only.join(", ")
        ));
    };
    let pkg = pkg.clone();
    if is_maintenance_project(&a.project) {
        return Err(eyre!(maintenance_refusal(&a.project, &pkg)));
    }
    let input = bugfix::parse_bug_input(
        &a.bug,
        &a.cve,
        a.description.as_deref(),
        a.bug_file.as_deref(),
    )
    .map_err(|e| eyre!(e))?;
    for w in &input.warnings {
        eprintln!("warning: {w}");
    }
    // Before any lookup: a host the user allows here is read from at once.
    a.allow_host.apply()?;
    let branch_project = match &a.branch_project {
        Some(b) => b.clone(),
        None => {
            let user = ap_build::osc::current_user(Emit::quiet()).map_err(|e| eyre!("{e}"))?;
            ap_build::osc::default_branch_project(&user, &a.project)
        }
    };
    let spec = ap_build::update::spec_for_package(&a.project, &pkg, Emit::quiet())
        .map_err(|e| eyre!("{pkg} in {}: {e}", a.project))?;
    let version = ap_build::spec::parse_header(&spec)
        .map(|h| h.version)
        .ok_or_else(|| {
            eyre!(
                "the spec of {pkg} in {} has no Name: and Version:",
                a.project
            )
        })?;
    let declared: Vec<String> = ap_build::patch_check::declared_patches(&spec)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let job = bugfix::prepare_fix(
        &FixRequest {
            pkg: &pkg,
            input: &input,
            commit_urls: &a.commit_url,
            commit_repo: a.commit_repo.as_deref(),
            upstream_repo: bugfix::upstream_repo_of_spec(&spec),
            declared_patches: &declared,
            version: &version,
        },
        &ap_build::allowed_hosts::shared(),
        &LiveNet,
    )
    .map_err(|e| eyre!(e))?;
    // Several upstream fixes and none chosen: a guess between commits is a
    // maintainer's call, so nothing is branched.
    if !job.candidates.is_empty() {
        for line in bugfix::dry_run_lines(&job, &version, &branch_project, &a.project, a.submit) {
            println!("{line}");
        }
        return Err(FixExit {
            code: EXIT_NEEDS_MAINTAINER,
            message: "several upstream fixes were found: pass the one(s) to use with --commit-url"
                .to_string(),
        }
        .into());
    }
    let nonce = job.nonce.clone();
    let state_root = a
        .work_dir
        .join(format!(
            "{}-{}",
            ap_core::run::sanitize_name(&branch_project),
            ap_core::run::sanitize_name(&a.arch)
        ))
        .join("state");
    let dry_run = a.dry_run;
    let update = update_args(&a, branch_project, Some(FixTask::Bug(job)));
    let result = crate::update_project::run(update, summary);
    if dry_run {
        return result;
    }
    let end = ap_build::workspace::Workspace::find_by_obs_name(&state_root, &pkg)
        .and_then(|ws| bugfix::read_fix_end(ws.dir(), &nonce));
    match (ending(result.is_ok(), end.as_ref()), result) {
        (Ending::Fixed, result) => result,
        (Ending::Exit(code, message), _) => Err(FixExit { code, message }.into()),
        (Ending::Failed, Err(e)) => Err(e),
        (Ending::Failed, Ok(())) => {
            Err(eyre!("the fix ended without a verdict: see the run's log"))
        }
    }
}
