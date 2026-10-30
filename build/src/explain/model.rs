use super::*;

/// The agent budget per package when the run's command line does not give one
/// (the default of `--budget-usd`).
pub const DEFAULT_BUDGET_USD: f64 = 10.0;

pub(super) const REASONING_MAX: usize = 700;

pub(super) const EVIDENCE_MAX: usize = 260;

pub(super) const EVIDENCE_LINES: usize = 5;

pub(super) const LOG_ERROR_LINES: usize = 8;

pub(super) const NOTES_LINES: usize = 60;

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Explanation {
    pub package: String,
    pub project_dir: String,
    /// The run the report is about (`None`: no run touched the package).
    pub run: Option<String>,
    /// Other runs that touched the package, newest first.
    pub other_runs: Vec<String>,
    pub run_info: RunInfo,
    pub version: VersionInfo,
    pub outcome: Outcome,
    pub attempts: Vec<Attempt>,
    pub flaky: Vec<String>,
    pub ladder: Ladder,
    pub cost: Cost,
    pub warnings: Vec<WarningLine>,
    pub skipped: Vec<String>,
    pub notes: Notes,
    /// Things the report cannot tell, said plainly.
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct RunInfo {
    pub command: Option<String>,
    /// The auto-package build that wrote the run; `None` for logs from before
    /// it was recorded.
    pub build: Option<String>,
    /// `Success`, `Failed`, ...; `None` when the run wrote no `run_finished`.
    pub finished: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub unreadable_lines: usize,
    /// The hosts the run allowed (`--allow-host` or the TUI), as the log says.
    pub hosts_allowed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct VersionInfo {
    pub from: Option<String>,
    pub to: Option<String>,
    pub reason: Option<String>,
    pub reason_raw: Option<String>,
    pub blocked_by: Vec<String>,
    /// How many times the run selected the package (more than once: it came
    /// back after its dependencies were built).
    pub selections: usize,
    /// `--revision` / `--to-version` from the run's command line.
    pub revision: Option<String>,
    pub forced_version: Option<String>,
    /// `Version:` of the spec in the checkout now.
    pub spec_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Outcome {
    /// `built`, `already built`, `failed`, `blocked`, `missing dependencies`
    /// or `no result recorded`.
    pub result: String,
    pub detail: Option<String>,
    /// `last_outcome` and `last_failure_reason` of state.json (the latest
    /// run's view, not necessarily this run's).
    pub state_outcome: Option<String>,
    pub state_reason: Option<String>,
    pub log_source: Option<String>,
    pub log_errors: Vec<String>,
    /// The newest changelog entry of the checkout.
    pub changelog: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Attempt {
    pub step: u64,
    pub kind: String,
    pub attempt: Option<u64>,
    pub round: Option<u64>,
    pub detail: Option<String>,
    pub outcome: Option<String>,
    pub elapsed_ms: Option<u64>,
    /// Ordinal among the agent turns of the run for this package.
    pub agent_turn: Option<usize>,
    pub agent: Option<Agent>,
    pub spec_added: Option<u64>,
    pub spec_removed: Option<u64>,
    pub rejected: Option<String>,
    pub build: Option<BuildInfo>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Agent {
    pub task: Option<String>,
    pub action: Option<String>,
    pub reasoning: Option<String>,
    pub evidence: Vec<String>,
    pub confidence: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct BuildInfo {
    pub classification: Option<String>,
    pub missing: Vec<String>,
    pub extracts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Ladder {
    pub max_round: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Cost {
    /// Sum of the agent turns and package-notes turns of this run's events.
    pub total_usd: f64,
    pub turns: usize,
    pub budget_usd: f64,
    /// Whether the budget is the default (the command line did not say).
    pub budget_is_default: bool,
    /// state.json's spend, when it belongs to this run.
    pub state_spent_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct WarningLine {
    pub step: Option<u64>,
    pub message: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Notes {
    /// Headings of state/<pkg>/notes.md (one per attempt it records).
    pub run_note_headings: Vec<String>,
    /// <notes dir>/<pkg>.md: what the tool keeps across runs.
    pub package_notes: String,
}

/// What to explain.
pub struct Query<'a> {
    /// The per-project directory holding `state/`, `runs/` and the checkout.
    pub project_dir: &'a Path,
    pub package: &'a str,
    /// A run id; `None` for the latest run that touched the package.
    pub run: Option<&'a str>,
    /// The text of `<notes dir>/<pkg>.md` (empty when there is none).
    pub package_notes: String,
}
