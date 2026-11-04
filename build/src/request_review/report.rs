//! The report of a review: what was read, the findings of each action, the
//! agent's opinion apart, and the exit code the findings give.

use serde::Serialize;

use super::gather::Skipped;
use super::request::{ActionSource, ActionTarget};
use super::{Finding, Severity};

/// `schema` changes only when a field changes meaning.
pub const SCHEMA: u32 = 1;

/// What the saved text and JSON say about the agent's words.
pub const AGENT_NOTE: &str = "The model's text is its own summary, not ground truth.";

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: u32,
    /// The build of auto-package that made the report.
    pub auto_package: String,
    pub request: RequestSummary,
    pub actions: Vec<ActionReport>,
    /// `None` when `--agent` was not given.
    pub agent: Option<AgentSection>,
    pub fail_on: Severity,
    pub fail_on_agent: bool,
    pub exit: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestSummary {
    pub id: String,
    pub state: String,
    pub creator: String,
    pub created: String,
    pub description: String,
    /// The findings about the request as a whole.
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionReport {
    pub source: ActionSource,
    pub target: ActionTarget,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
}

/// The agent's turn: never mixed with the checks' findings.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSection {
    pub ran: bool,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub error: Option<String>,
    pub note: &'static str,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Add the agent's section and settle the exit code again: the agent's
    /// findings count only with `fail_on_agent`.
    pub fn set_agent(&mut self, section: AgentSection) {
        self.agent = Some(section);
        self.exit = exit_code(self);
    }

    /// Every finding of a check, the request's and the actions'.
    pub fn check_findings(&self) -> impl Iterator<Item = &Finding> {
        self.request
            .findings
            .iter()
            .chain(self.actions.iter().flat_map(|a| &a.findings))
    }

    /// The findings that decide the exit code: the checks', and the agent's
    /// only with `fail_on_agent`.
    fn deciding(&self) -> impl Iterator<Item = &Finding> {
        let agent = self
            .agent
            .iter()
            .filter(|_| self.fail_on_agent)
            .flat_map(|a| &a.findings);
        self.check_findings().chain(agent)
    }

    /// How many findings are at or above `fail_on`.
    pub fn failing(&self) -> usize {
        self.deciding()
            .filter(|f| f.severity >= self.fail_on)
            .count()
    }
}

/// Whether `--fail-on-agent` is asked and the agent's turn did not run: its
/// silence cannot pass for "nothing found".
pub fn agent_failed_when_it_decides(report: &Report) -> bool {
    report.fail_on_agent && report.agent.as_ref().is_some_and(|a| !a.ran)
}

/// The exit code of a review that ran: 1 when a deciding finding is at or
/// above `fail_on`, else 0. The agent's findings count only with
/// `fail_on_agent`, and with it a turn that did not run (budget, rate limit, a
/// bad answer, a crash) is exit status 2: the review could not be completed.
pub fn exit_code(report: &Report) -> i32 {
    if agent_failed_when_it_decides(report) {
        2
    } else {
        i32::from(report.failing() > 0)
    }
}
