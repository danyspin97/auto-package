use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunOutcome {
    Success,
    Failed,
    Aborted,
}

impl std::fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunOutcome::Success => write!(f, "Success"),
            RunOutcome::Failed => write!(f, "Failed"),
            RunOutcome::Aborted => write!(f, "Aborted"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepKind {
    Resolve,
    SelectTarget,
    Checkout,
    Commit,
    OscService,
    FetchSdist,
    InspectSdist,
    GenerateSpec,
    ValidateSpec,
    Build,
    ExtractFailure,
    AgentTurn,
    ApplyAndValidate,
}

impl std::fmt::Display for StepKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            StepKind::Resolve => "Resolve",
            StepKind::SelectTarget => "SelectTarget",
            StepKind::Checkout => "Checkout",
            StepKind::Commit => "Commit",
            StepKind::OscService => "OscService",
            StepKind::FetchSdist => "FetchSdist",
            StepKind::InspectSdist => "InspectSdist",
            StepKind::GenerateSpec => "GenerateSpec",
            StepKind::ValidateSpec => "ValidateSpec",
            StepKind::Build => "Build",
            StepKind::ExtractFailure => "ExtractFailure",
            StepKind::AgentTurn => "AgentTurn",
            StepKind::ApplyAndValidate => "ApplyAndValidate",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepOutcome {
    Ok,
    Failed,
    Skipped,
}

impl std::fmt::Display for StepOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepOutcome::Ok => write!(f, "ok"),
            StepOutcome::Failed => write!(f, "failed"),
            StepOutcome::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectionReason {
    RequestedRoot,
    FirstUnbuiltDependency,
    ExplicitOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BuildFailure {
    RpmParseError { line: Option<u32>, message: String },
    PrepFailed,
    BuildFailed,
    InstallFailed,
    CheckFailed,
    UnresolvableDeps { packages: Vec<String> },
    Unclassified { raw: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpecRejection {
    MacroInComment { line: u32, text: String },
    RpmspecParseFailed { stderr: String },
    MissingSourceFiles { names: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MissingSource {
    BuildLog,
    Agent,
}

/// Final result of building one package, as seen by the CLI's build loop.
/// Unlike per-step events, this is not tied to a `StepId` — it is reported
/// once a package's thread has been joined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageOutcome {
    Built,
    AlreadyBuilt,
    MissingDeps { deps: Vec<String> },
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Task {
    Review,
    FixAfterBuild { attempt: u32, max_attempts: u32 },
}

impl std::fmt::Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Task::Review => write!(f, "review"),
            Task::FixAfterBuild {
                attempt,
                max_attempts,
            } => {
                write!(f, "fix {attempt}/{max_attempts}")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponseData {
    pub action: String,
    pub reasoning: String,
    pub evidence: Vec<String>,
    pub confidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extract {
    pub genre: Option<String>,
    pub phase: Option<String>,
    pub located_by: Option<String>,
    pub excerpt: String,
    pub error_lines: Vec<String>,
    pub elided_count: usize,
    pub source_candidates: Vec<PathBuf>,
    pub has_signal: bool,
}

fn serialize_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(d.as_millis() as u64)
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    use serde::Deserialize;
    let ms = u64::deserialize(d)?;
    Ok(Duration::from_millis(ms))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        run_id: RunId,
        argv: Vec<String>,
        work_dir: PathBuf,
    },
    RunFinished {
        outcome: RunOutcome,
        #[serde(
            serialize_with = "serialize_duration",
            deserialize_with = "deserialize_duration"
        )]
        elapsed: Duration,
        usage: Usage,
    },
    ResolveStarted {
        root: String,
    },
    ResolveFinished {
        total: usize,
        built: usize,
        remaining: usize,
    },
    TargetSelected {
        pkg: String,
        version: String,
        reason: SelectionReason,
        blocked_by: Vec<String>,
    },
    StepStarted {
        step: StepId,
        kind: StepKind,
    },
    StepFinished {
        step: StepId,
        outcome: StepOutcome,
        #[serde(
            serialize_with = "serialize_duration",
            deserialize_with = "deserialize_duration"
        )]
        elapsed: Duration,
    },
    Command {
        step: Option<StepId>,
        program: String,
        argv: Vec<String>,
        cwd: PathBuf,
        log: Option<PathBuf>,
    },
    CommandDone {
        step: Option<StepId>,
        code: i32,
        #[serde(
            serialize_with = "serialize_duration",
            deserialize_with = "deserialize_duration"
        )]
        elapsed: Duration,
        stdout_bytes: u64,
        stderr_bytes: u64,
    },
    BuildFailed {
        step: StepId,
        classification: BuildFailure,
        log: PathBuf,
    },
    ExtractResult {
        step: StepId,
        extract: Extract,
        artifact: PathBuf,
    },
    AgentPrompt {
        step: StepId,
        task: Task,
        prompt: String,
        artifact: PathBuf,
    },
    /// Agent turn completed. Named AgentReply to avoid collision with AgentResponseData.
    AgentReply {
        step: StepId,
        response: AgentResponseData,
        raw: String,
        usage: Usage,
        artifact: PathBuf,
    },
    AgentToolUse {
        step: StepId,
        tool: String,
        input: String,
    },
    SpecChanged {
        step: StepId,
        diff: String,
        lines_added: usize,
        lines_removed: usize,
        artifact: PathBuf,
    },
    SpecRejected {
        step: StepId,
        reason: SpecRejection,
        restored_from: PathBuf,
    },
    MissingPackages {
        step: StepId,
        names: Vec<String>,
        source: MissingSource,
    },
    /// Normal forward progress that takes long enough to be worth narrating,
    /// e.g. a remote build changing state. Not a problem — see `Warning`.
    Progress {
        step: Option<StepId>,
        message: String,
    },
    Warning {
        step: Option<StepId>,
        message: String,
    },
    Artifact {
        step: StepId,
        name: String,
        path: PathBuf,
    },
    PackageFinished {
        pkg: String,
        version: String,
        outcome: PackageOutcome,
    },
}
