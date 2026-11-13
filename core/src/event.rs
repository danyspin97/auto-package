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
    /// The user stopped the run. What it left unfinished is not a failure.
    Interrupted,
}

impl std::fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunOutcome::Success => write!(f, "Success"),
            RunOutcome::Failed => write!(f, "Failed"),
            RunOutcome::Aborted => write!(f, "Aborted"),
            RunOutcome::Interrupted => write!(f, "Interrupted"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Variants are in canonical pipeline order; `STEPS` in `step.rs` is indexed
/// by discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepKind {
    Checkout,
    OscService,
    GenerateSpec,
    FetchSdist,
    ZigVendor,
    ValidateSpec,
    Commit,
    Build,
    ExtractFailure,
    AgentTurn,
    PackageNotes,
}

impl std::fmt::Display for StepKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.spec().label)
    }
}

impl StepKind {
    /// Human-readable label for display to end users (TUI, text logs).
    pub fn label(&self) -> &'static str {
        self.spec().label
    }
}

fn default_attempt() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    NotRust,
    ServiceUpToDate,
    SpecExists,
    NoFailure,
    NoAgent,
    BudgetExhausted,
    Disabled,
    Other(String),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::NotRust => f.write_str("not a Rust package"),
            SkipReason::ServiceUpToDate => f.write_str("source service output is up to date"),
            SkipReason::SpecExists => f.write_str("spec already exists"),
            SkipReason::NoFailure => f.write_str("no failure to analyze"),
            SkipReason::NoAgent => f.write_str("agent disabled (--no-agent)"),
            SkipReason::BudgetExhausted => f.write_str("budget exhausted"),
            SkipReason::Disabled => f.write_str("disabled"),
            SkipReason::Other(why) => f.write_str(why),
        }
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
    RpmParseError {
        line: Option<u32>,
        message: String,
    },
    PrepFailed,
    BuildFailed,
    InstallFailed,
    CheckFailed,
    /// rpm's post-build check found directories no package owns: `paths` are
    /// the unowned directories, in the order the log names them.
    FilesNotOwned {
        paths: Vec<String>,
    },
    UnresolvableDeps {
        packages: Vec<String>,
    },
    Unclassified {
        raw: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpecRejection {
    MacroInComment {
        line: u32,
        text: String,
    },
    RpmspecParseFailed {
        stderr: String,
    },
    MissingSourceFiles {
        names: Vec<String>,
    },
    /// The spec still has `TODO` placeholders, as `line: text`.
    Placeholders {
        lines: Vec<String>,
    },
    /// The edit declared these sources again (the source loop guard).
    RepeatedSource {
        urls: Vec<String>,
    },
    /// The edit put a `%python_expand` line inside a braced `%{python_expand ...}`
    /// block (convention 19 (section 80)), as `line N: text`.
    NestedPythonExpand {
        lines: Vec<String>,
    },
    /// The edit reshaped a braced `%{python_expand ...}` block the origin has
    /// (convention 19 (section 119)), named by the origin's first line.
    ReshapedPythonExpandBlock {
        first_lines: Vec<String>,
    },
    /// The edit adds a fixup of the sources or the build (`sed -i`, `chmod`, `rm`,
    /// `export`, ...) without a comment above it that says why (convention 40), as
    /// `line N (text) problem`.
    UnexplainedFixup {
        lines: Vec<String>,
    },
    /// The edit left an rpmlintrc with lines convention 39 does not allow, as
    /// `file: line N (...) ...`.
    BadRpmlintrc {
        problems: Vec<String>,
    },
    /// An update's `Source0` went from the release's sdist to its wheel
    /// (convention 32); `from` and `to` are the two values.
    SdistToWheel {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MissingSource {
    BuildLog,
    Agent,
}

pub use crate::package_outcome::PackageOutcome;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Task {
    Review,
    FixAfterBuild {
        attempt: u32,
        max_attempts: u32,
    },
    /// One-off, read-only lookup of upstream names for OBS's unresolvable
    /// dependency list — not counted against `FixAfterBuild`'s attempts.
    TranslateUnresolvableDeps,
    /// Patching a distro package branched into the project because another
    /// package needs a change to it.
    PatchDependency,
    /// One cheap look at a spec that has just built, to drop leftovers of the
    /// failed attempts that no longer serve a purpose.
    PostBuildReview,
    /// A fresh look at one rewritten patch.
    PatchReview,
    /// Fixing a reported bug in the package at its current version
    /// (`auto-package fix`).
    FixBug,
    /// Fixing the findings of a review of a submit request
    /// (`auto-package fix --request`).
    FixFindings,
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
            Task::TranslateUnresolvableDeps => write!(f, "translate deps"),
            Task::PatchDependency => write!(f, "patch dependency"),
            Task::PostBuildReview => write!(f, "post-build review"),
            Task::PatchReview => write!(f, "patch review"),
            Task::FixBug => write!(f, "fix bug"),
            Task::FixFindings => write!(f, "fix findings"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponseData {
    pub action: String,
    pub reasoning: String,
    pub evidence: Vec<String>,
    pub confidence: Option<String>,
    /// The turn's changelog entry; empty when it gave none.
    #[serde(default)]
    pub changelog: String,
    /// A refresh turn's answer for each patch it touched; empty otherwise.
    #[serde(default)]
    pub patches: Vec<PatchAnswerData>,
}

/// One patch's answer in a refresh turn: its name, its status and the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchAnswerData {
    pub name: String,
    pub status: String,
    pub reason: String,
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
    /// The build log says the disk is full (`ENOSPC`).
    #[serde(default)]
    pub disk_full: bool,
}

fn serialize_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(d.as_millis() as u64)
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    use serde::Deserialize;
    let ms = u64::deserialize(d)?;
    Ok(Duration::from_millis(ms))
}

/// How long an allowed host lasts (see [`Event::HostAllowed`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostScope {
    #[default]
    Run,
    Persistent,
}

/// What allowed a host (see [`Event::HostAllowed`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSource {
    #[default]
    Flag,
    Tui,
}

/// One result of an [`Event::ArchCheck`]. `state` is the summary's label
/// (`succeeded`, `failed`, `still building`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchCheckResult {
    pub repo: String,
    pub arch: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The same result fails in the origin project too.
    #[serde(default)]
    pub origin_fails: bool,
    /// The origin project (Factory, for a port) has no result of the package
    /// there, so the failure is a note and does not block a submit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub origin_missing: bool,
    /// The saved build log of a failure, when it was fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        run_id: RunId,
        argv: Vec<String>,
        work_dir: PathBuf,
        /// The auto-package build that wrote this run (`build_info`); absent
        /// in logs written before it was recorded.
        #[serde(default)]
        build: String,
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
        in_distro: usize,
        remaining: usize,
    },
    TargetSelected {
        pkg: String,
        version: String,
        reason: SelectionReason,
        blocked_by: Vec<String>,
        /// For an update: the version the package was at before it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_version: Option<String>,
    },
    StepStarted {
        step: StepId,
        kind: StepKind,
        /// 1-based count of this kind's starts within the package run.
        #[serde(default = "default_attempt")]
        attempt: u32,
        /// Iteration of the outer validate/build loop.
        #[serde(default)]
        round: u32,
        /// Per-call qualifier, e.g. which agent task a turn is for.
        #[serde(default)]
        detail: Option<String>,
    },
    /// A conditional step that did not start, so there is no `StepId`.
    StepSkipped {
        kind: StepKind,
        reason: SkipReason,
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
    /// Package-notes turn completed. Its cost counts toward the package and run
    /// totals, but it is not an agent reply, so it has its own event.
    NotesUsage {
        step: StepId,
        usage: Usage,
    },
    /// An agent turn that failed (it crashed, timed out or ran out of turns)
    /// still cost money, and has no reply: its cost is counted with its own
    /// event, so the run's turns and cost add up.
    FailedTurnUsage {
        step: StepId,
        usage: Usage,
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
    /// The `Version:` of a package could not be checked against the version its
    /// tag gives when OBS builds it (a `Warning` says why). The run summary
    /// lists these; nothing else reads them.
    VersionUnchecked {
        pkg: String,
        reason: String,
    },
    /// The sweep of an `update` for newer releases, as far as it has got: how
    /// many packages it has compared so far and what it found. `finished`
    /// once every package is compared. `package` is the package whose check
    /// this report counts, when it is for one.
    SweepProgress {
        checked: usize,
        to_update: usize,
        up_to_date: usize,
        skipped: usize,
        #[serde(default)]
        finished: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<crate::sweep_event::SweptPackage>,
        /// How many packages the sweep will compare in all, once it is known
        /// (the `checked` count out of it). Absent in logs from before it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
    },
    /// The sweep has begun the check of `package`, by its OBS name. Its row
    /// reads `checking` until the `SweepProgress` that answers it. Only the
    /// TUI reads it.
    SweepChecking {
        package: String,
    },
    /// The other-architecture check of an updated package: one entry per
    /// checked `(repo, arch)`, the failing ones with their detail.
    ArchCheck {
        package: String,
        #[serde(default)]
        results: Vec<ArchCheckResult>,
    },
    /// A host was allowed for the services and source fetches of this tool.
    /// `scope` is `run` (this process only) or `persistent` (also written to
    /// the user's file); `source` is `flag` (`--allow-host`) or `tui`.
    HostAllowed {
        host: String,
        #[serde(default)]
        scope: HostScope,
        #[serde(default)]
        source: HostSource,
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
    /// The update preparation dropped a package it had queued for the TUI, so
    /// nothing was built for it. Its outcome is what the preparation said
    /// (skipped or failed). Only the TUI reports it: unlike `PackageFinished`,
    /// it does not keep the run open.
    PackageDropped {
        pkg: String,
        version: String,
        outcome: PackageOutcome,
    },
    /// The zig vendor archive of a package was rebuilt from the `build.zig.zon`
    /// of its new source. `changed` is false when the archive was already that.
    /// `deps` are the dependencies it holds, in the order they are written.
    ZigVendor {
        step: StepId,
        pkg: String,
        file: String,
        changed: bool,
        deps: Vec<ZigDep>,
    },
    /// One patch review round's verdict: `accepted`, `rewritten`, `rejected`,
    /// `gave_up`, `rolled_back`, `slip` or `not_reviewed` (no turn could start).
    PatchReviewed {
        step: StepId,
        pkg: String,
        patch: String,
        round: u32,
        verdict: String,
        reason: String,
    },
    /// One record of `auto-package fix` (see [`BugFixEvent`]).
    BugFix(crate::fix_event::BugFixEvent),
    /// What `auto-package fix --request` did with one finding of the review
    /// (see [`FindingFixEvent`]).
    FindingFix(crate::fix_event::FindingFixEvent),
}

/// One dependency a zig vendor archive holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZigDep {
    /// The name the manifest gives the dependency.
    pub name: String,
    /// The URL the manifest gives it.
    pub url: String,
    /// The hash the manifest declares, `name-version-<digest>`.
    pub hash: String,
}
