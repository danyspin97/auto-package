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
    /// Built on this machine by `--local`: OBS has no build of it.
    BuiltLocally,
    AlreadyBuilt,
    MissingDeps {
        deps: Vec<String>,
    },
    Failed {
        reason: String,
        /// The failure was a full disk, so the user can free space and retry.
        #[serde(default)]
        disk_full: bool,
    },
    /// Stopped for a maintainer's decision (see `FailureKind::NeedsMaintainer`):
    /// nothing was sent to OBS, and it is not a build failure.
    NeedsMaintainer {
        reason: String,
    },
}

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

#[cfg(test)]
mod host_allowed_tests {
    use super::*;

    #[test]
    fn host_allowed_has_its_wire_form_and_defaults_when_fields_are_missing() {
        let out = serde_json::to_string(&Event::HostAllowed {
            host: "x.example.org".into(),
            scope: HostScope::Persistent,
            source: HostSource::Tui,
        })
        .unwrap();
        assert_eq!(
            out,
            r#"{"event":"host_allowed","host":"x.example.org","scope":"persistent","source":"tui"}"#
        );
        let e: Event =
            serde_json::from_str(r#"{"event":"host_allowed","host":"a.example.org"}"#).unwrap();
        assert!(matches!(
            e,
            Event::HostAllowed {
                ref host,
                scope: HostScope::Run,
                source: HostSource::Flag,
            } if host == "a.example.org"
        ));
    }
}

#[cfg(test)]
mod arch_check_tests {
    use super::*;

    #[test]
    fn arch_check_has_its_wire_form_and_old_logs_still_parse() {
        let e = Event::ArchCheck {
            package: "foo".into(),
            results: vec![ArchCheckResult {
                repo: "ports".into(),
                arch: "s390x".into(),
                state: "failed".into(),
                detail: None,
                origin_fails: false,
                log: None,
            }],
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"event":"arch_check","package":"foo","results":[{"repo":"ports","arch":"s390x","state":"failed","origin_fails":false}]}"#
        );
        let old: Event = serde_json::from_str(r#"{"event":"arch_check","package":"foo"}"#).unwrap();
        assert!(matches!(old, Event::ArchCheck { ref results, .. } if results.is_empty()));
        let bare: ArchCheckResult =
            serde_json::from_str(r#"{"repo":"ports","arch":"s390x","state":"succeeded"}"#).unwrap();
        assert!(!bare.origin_fails && bare.detail.is_none() && bare.log.is_none());
    }
}
