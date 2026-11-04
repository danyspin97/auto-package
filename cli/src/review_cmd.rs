//! The `review` sub-command: a read-only review of one OBS submit request.
//!
//! It reads the request, its sources and the build results, runs the
//! deterministic checks and prints the report. It never changes OBS: no
//! request is accepted, declined or commented on, and nothing is committed.
//! (Posting the report as a comment on the request is a possible later flag,
//! and would be the command's only write.)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ap_build::proc::Emit;
use ap_build::request_review::Severity;
use ap_build::request_review::agent_turn::{
    AgentOptions, agent_review, forge_hosts, live_preflight,
};
use ap_build::request_review::fetch::LiveRequestObs;
use ap_build::request_review::render::{render_json, render_text};
use ap_build::request_review::run::{Deps, Options, review};
use ap_build::request_review::save::{review_dir, save};
use ap_build::request_review::upstream_check::{live_plan, live_repology};
use ap_core::sink::{HumanSink, LogLevel, MemorySink};
use clap::{Args, ValueEnum};

/// Exit status of a review that could not run.
const EXIT_ERROR: i32 = 2;

#[derive(Args)]
pub(super) struct ReviewArgs {
    /// The id of the submit request to review (digits).
    #[arg(value_parser = parse_request_id)]
    request_id: String,

    /// How the report is printed on stdout.
    #[arg(long, value_enum, default_value = "text")]
    format: Format,

    /// The lowest severity of a finding that makes the exit status 1.
    #[arg(long, value_enum, default_value = "warning")]
    fail_on: FailOn,

    /// Where the report is saved, under `reviews/<request-id>/<run-id>/`.
    #[arg(long, default_value = "ap-work")]
    work_dir: PathBuf,

    /// Add one read-only model turn that reads the request's files for what the
    /// checks cannot say. Its findings are labelled as opinion and are printed
    /// apart; they never change the exit status unless `--fail-on-agent` is
    /// given. It may read the upstream forge host of the source, only when the
    /// allowed-hosts list (`hosts allow`) already allows that host.
    #[arg(long)]
    agent: bool,

    /// The most the agent turn may spend, in USD (needs `--agent`).
    #[arg(long, default_value_t = 2.0, requires = "agent", value_parser = parse_budget)]
    budget_usd: f64,

    /// Let the agent's findings at or above `--fail-on` make the exit status 1
    /// (needs `--agent`). Without it they never do.
    #[arg(long, requires = "agent")]
    fail_on_agent: bool,

    /// Do not download source archives: the checks that need their bytes
    /// (signatures, patch paths, a packed `target/`) are reported as not run.
    #[arg(long)]
    no_download: bool,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    log: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum FailOn {
    Blocker,
    Warning,
    Note,
}

impl FailOn {
    fn severity(self) -> Severity {
        match self {
            FailOn::Blocker => Severity::Blocker,
            FailOn::Warning => Severity::Warning,
            FailOn::Note => Severity::Note,
        }
    }
}

/// A request id is digits only; it is checked before any call is made.
fn parse_request_id(id: &str) -> Result<String, String> {
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
        Ok(id.to_string())
    } else {
        Err(format!("{id:?} is not a request id: digits only"))
    }
}

/// A budget in USD: a number above zero.
fn parse_budget(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(usd) if usd.is_finite() && usd > 0.0 => Ok(usd),
        _ => Err(format!("{text:?} is not a budget in USD above zero")),
    }
}

/// Run the review and give the exit status: 0 or 1 by the findings, 2 when it
/// could not run.
pub(super) fn main(args: &ReviewArgs) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_ERROR
        }
    }
}

/// The target repository's `%ldconfig_scriptlets` support, read from OBS.
pub(crate) fn ldconfig_macro(target_project: &str) -> bool {
    let (repo, _) = ap_build::osc::choose_repository(target_project, None, "x86_64");
    ap_build::spec_tidy::hygiene::ldconfig_macro_defined(&repo)
}

fn run(args: &ReviewArgs) -> Result<i32, String> {
    let level: LogLevel = args.log.parse()?;
    // Before any read, so a review that cannot run its turn writes no report.
    if args.agent {
        live_preflight(&std::path::absolute(&args.work_dir).map_err(|e| e.to_string())?)?;
    }
    super::install_interrupt_handler(false, Arc::new(MemorySink::new()))
        .map_err(|e| e.to_string())?;
    let sink = HumanSink::new(level);
    let obs = LiveRequestObs {
        emit: Emit::run(&sink),
    };
    let work_dir = std::path::absolute(&args.work_dir).map_err(|e| e.to_string())?;
    let run_id = ap_core::run::new_run_id();
    let dir = review_dir(&work_dir, &args.request_id, &run_id.0);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let scratch = dir.join("sources");
    let plan = live_plan(Emit::run(&sink));
    // The lookups of a spec from a request go only to hosts the list allows.
    let hosts = ap_build::allowed_hosts::load_default();
    let host_allowed = |host: &str| hosts.allows(host, ap_build::allowed_hosts::Use::Source);
    let deps = Deps {
        plan: &plan,
        repology: &live_repology,
        ldconfig_macro: &ldconfig_macro,
        host_allowed: &host_allowed,
    };
    let options = Options {
        id: &args.request_id,
        download: !args.no_download,
        scratch: &scratch,
        fail_on: args.fail_on.severity(),
        fail_on_agent: args.fail_on_agent,
        build: env!("AP_BUILD_REVISION"),
    };
    let result = review(&obs, &options, &deps);
    // The downloads are the review's scratch, not run data.
    let _ = std::fs::remove_dir_all(&scratch);
    let mut reviewed = match result {
        Ok(r) => r,
        Err(e) => {
            discard(&dir);
            return Err(e.to_string());
        }
    };
    if args.agent {
        let hosts = ap_build::allowed_hosts::load_default();
        let web_hosts = forge_hosts(&reviewed, &hosts);
        let section = agent_review(
            &reviewed,
            &AgentOptions {
                budget_usd: args.budget_usd,
                work_dir: &work_dir,
                dir: &dir,
                web_hosts: &web_hosts,
            },
            &|req, hosts| ap_build::agent::turn_with_hosts(req, hosts),
        );
        reviewed.report.set_agent(section);
    }
    let text = render_text(&reviewed.report);
    let json = render_json(&reviewed.report);
    save(&dir, &reviewed, &text, &json).map_err(|e| format!("cannot save the review: {e}"))?;
    match args.format {
        Format::Text => print!("{text}"),
        Format::Json => print!("{json}"),
    }
    eprintln!("Saved: {}", dir.display());
    Ok(reviewed.report.exit)
}

/// Remove the directory of a review that did not run, and its request
/// directory when nothing else is in it: an error writes no report.
fn discard(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        // Only succeeds when empty.
        let _ = std::fs::remove_dir(parent);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: ReviewArgs,
    }

    fn parse(args: &[&str]) -> Result<Wrapper, clap::Error> {
        Wrapper::try_parse_from(std::iter::once("x").chain(args.iter().copied()))
    }

    #[test]
    fn the_request_id_is_required_and_digits_only() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["12a"]).is_err());
        assert!(parse(&["../1"]).is_err());
        assert_eq!(parse(&["1394662"]).unwrap().args.request_id, "1394662");
    }

    #[test]
    fn the_defaults_fail_on_warning_and_print_text() {
        let a = parse(&["1"]).unwrap().args;
        assert_eq!(a.fail_on.severity(), Severity::Warning);
        assert!(matches!(a.format, Format::Text));
        assert!(!a.no_download);
        assert_eq!(a.work_dir.to_str(), Some("ap-work"));
    }

    #[test]
    fn the_threshold_and_format_take_their_values_and_refuse_others() {
        let a = parse(&[
            "1",
            "--fail-on",
            "blocker",
            "--format",
            "json",
            "--no-download",
        ])
        .unwrap()
        .args;
        assert_eq!(a.fail_on.severity(), Severity::Blocker);
        assert!(matches!(a.format, Format::Json));
        assert!(a.no_download);
        assert!(parse(&["1", "--fail-on", "error"]).is_err());
        assert!(parse(&["1", "--format", "xml"]).is_err());
    }

    #[test]
    fn the_agent_flags_need_agent_and_a_budget_above_zero() {
        assert!(parse(&["1", "--budget-usd", "1"]).is_err());
        assert!(parse(&["1", "--fail-on-agent"]).is_err());
        let a = parse(&["1", "--agent"]).unwrap().args;
        assert!(a.agent && !a.fail_on_agent);
        assert_eq!(a.budget_usd, 2.0);
        let a = parse(&["1", "--agent", "--budget-usd", "0.5", "--fail-on-agent"])
            .unwrap()
            .args;
        assert_eq!(a.budget_usd, 0.5);
        assert!(a.fail_on_agent);
        for bad in ["0", "-1", "x", "nan"] {
            assert!(
                parse(&["1", "--agent", "--budget-usd", bad]).is_err(),
                "{bad}"
            );
        }
        assert!(!parse(&["1"]).unwrap().args.agent);
    }

    #[test]
    fn a_review_that_did_not_run_leaves_no_directory() {
        let work = tempfile::tempdir().unwrap();
        let dir = review_dir(work.path(), "7", "r1");
        std::fs::create_dir_all(dir.join("sources")).unwrap();
        discard(&dir);
        assert!(!work.path().join("reviews/7").exists());
        // Another review of the request keeps the request directory.
        let other = review_dir(work.path(), "7", "r0");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        discard(&dir);
        assert!(other.exists());
    }
}
