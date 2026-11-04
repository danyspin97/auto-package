use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
    /// A step boundary (`Event::StepStarted`) — styled apart from ordinary
    /// log output so the event log reads as a sequence of named sections
    /// instead of one undifferentiated stream of lines.
    Step,
}

impl LogLevel {
    pub(super) fn color(self) -> Color {
        match self {
            LogLevel::Info => Color::White,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Error => Color::Red,
            LogLevel::Success => Color::Green,
            LogLevel::Step => Color::Cyan,
        }
    }
}

#[derive(Clone)]
pub(super) struct LogLine {
    pub(super) level: LogLevel,
    pub(super) message: String,
    /// Rendered bold — used for step-boundary headers so they stand out from
    /// the regular log lines between them.
    pub(super) emphasis: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum PkgStatus {
    Queued,
    Running,
    Succeeded,
    /// Built on this machine by `--local`: OBS has no build of it, so it is
    /// not offered for a submit request.
    BuiltLocally,
    AlreadyBuilt,
    MissingDeps,
    Failed,
    /// Live, but no step is executing right now: the last step has finished
    /// and the agent turn (or `PackageFinished`) has not yet come in.
    Between,
    /// `Failed` whose build log signature matched [`looks_like_disk_full`] —
    /// split out from plain `Failed` so it's visually distinct in the table
    /// (nothing about the spec is wrong; there's nothing to fix but disk
    /// space) and so the `r` keybinding knows which row it applies to.
    NoSpace,
    /// Excluded by OBS for this repository and arch: nothing was built, and
    /// the reason is in the detail pane.
    Skipped,
}

impl PkgStatus {
    pub(super) fn label(&self) -> &'static str {
        match self {
            PkgStatus::Queued => "queued",
            PkgStatus::Running => "running",
            PkgStatus::Between => "working",
            PkgStatus::Succeeded => "done",
            PkgStatus::BuiltLocally => "built locally",
            PkgStatus::AlreadyBuilt => "cached",
            PkgStatus::MissingDeps => "blocked",
            PkgStatus::Failed => "failed",
            PkgStatus::NoSpace => "no space",
            PkgStatus::Skipped => "skipped",
        }
    }

    /// A one-column symbol in front of the label, so a row's state reads at a
    /// glance and still shows without colour. `tick` animates the running
    /// symbol.
    pub(super) fn glyph(&self, tick: u64) -> &'static str {
        match self {
            PkgStatus::Queued => "○",
            PkgStatus::Running => ["◐", "◓", "◑", "◒"][(tick % 4) as usize],
            PkgStatus::Between => "◌",
            PkgStatus::Succeeded | PkgStatus::BuiltLocally => "✓",
            PkgStatus::AlreadyBuilt => "✔",
            PkgStatus::MissingDeps => "◇",
            PkgStatus::Skipped => "⊘",
            PkgStatus::Failed | PkgStatus::NoSpace => "✗",
        }
    }

    pub(super) fn color(&self) -> Color {
        match self {
            PkgStatus::Queued => Color::DarkGray,
            PkgStatus::Running | PkgStatus::Between => Color::Yellow,
            PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt => {
                Color::Green
            }
            PkgStatus::MissingDeps => Color::Magenta,
            PkgStatus::Failed => Color::Red,
            PkgStatus::NoSpace => Color::LightRed,
            PkgStatus::Skipped => Color::DarkGray,
        }
    }
}

/// What kind of thing an [`ActivityEntry`] records — lets the Agent Activity
/// pane style each kind differently instead of a single uniform color, since
/// a turn's start, its tool calls, and its final decision read very
/// differently in a wall of otherwise-identical text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityKind {
    /// A new agent turn starting (`Event::AgentPrompt`).
    Task,
    /// A tool call the agent made mid-turn (`Event::AgentToolUse`).
    ToolUse,
    /// The agent's final decision for the turn (`Event::AgentReply`).
    Reply,
}

impl ActivityKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            ActivityKind::Task => "Task",
            ActivityKind::ToolUse => "Tool",
            ActivityKind::Reply => "Reply",
        }
    }

    pub(super) fn color(self) -> Color {
        match self {
            ActivityKind::Task => Color::Cyan,
            ActivityKind::ToolUse => Color::Blue,
            ActivityKind::Reply => Color::Green,
        }
    }
}

#[derive(Clone)]
pub(super) struct ActivityEntry {
    pub(super) kind: ActivityKind,
    pub(super) text: String,
}

/// One step of a package's build, from `StepStarted` until it is finished.
pub(super) struct StepInfo {
    pub(super) kind: StepKind,
    /// 1-based start count of this kind; a retry shows as `#2`.
    pub(super) attempt: u32,
    /// Iteration of the validate/build loop the step ran in.
    pub(super) round: u32,
    pub(super) started: Instant,
    /// Outcome and the event's own elapsed time, set by `StepFinished` (or by
    /// `PackageFinished` if the worker died with the step still open).
    pub(super) done: Option<(StepOutcome, Duration)>,
}

pub(super) struct PkgState {
    pub(super) version: String,
    /// For an update: the version the package was at before it.
    pub(super) from_version: Option<String>,
    pub(super) status: PkgStatus,
    /// The last step seen, never cleared between steps; cleared only for a
    /// fresh attempt and for packages that finished without failing.
    pub(super) step: Option<StepInfo>,
    /// Reason from `PackageOutcome::Failed`, shown in the detail pane.
    pub(super) fail_reason: Option<String>,
    /// Conditional steps that did not run, in the order they were skipped.
    pub(super) skipped: Vec<(StepKind, SkipReason)>,
    pub(super) log_lines: VecDeque<LogLine>,
    /// Lines ever added, including ones since evicted from `log_lines`. Only
    /// grows, so a view can count arrivals even once the cap is hit.
    pub(super) log_seq: u64,
    pub(super) agent_activity: VecDeque<ActivityEntry>,
    /// Same as `log_seq`, for `agent_activity`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) activity_seq: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cost_usd: f64,
    pub(super) started: Instant,
    pub(super) finished: bool,
    /// Current/max agent fix-attempt count within this `run_package` call
    /// (`Task::FixAfterBuild`'s `attempt`/`max_attempts`). Stays `0`/`0`
    /// until the first post-build fix attempt — a package fixed on the
    /// first try, or still on its initial build, never needed one.
    pub(super) tries: u32,
    pub(super) max_tries: u32,
    /// Set once this package's build log has shown [`looks_like_disk_full`]'s
    /// signature. Sticky for the rest of the run: an `ExtractResult` only
    /// arrives for the attempt that just failed, but the eventual
    /// `PackageFinished` that turns this into a visible `PkgStatus::NoSpace`
    /// arrives later, after the agent's (likely futile) fix attempt — so
    /// this has to survive until then rather than being read inline.
    pub(super) disk_full_detected: bool,
    /// The user pressed `r` and the root request was sent; further presses
    /// are ignored until the package's status changes, so holding or
    /// repeating the key doesn't queue duplicate builds.
    pub(super) retry_requested: bool,
    /// Dependencies named by the last `PackageOutcome::MissingDeps`, sorted
    /// and deduplicated; empty once the package is retried or finishes any
    /// other way. Kept apart from the log so the Queued list can show it.
    pub(super) waiting_on: Vec<String>,
    /// The state of this package's submit request, once one was asked for.
    pub(super) sr: Option<SrMark>,
    /// The spec diffs the run recorded (`SpecChanged`), oldest first.
    pub(super) spec_diffs: Vec<String>,
}

/// Spec diffs kept per package for the diff view's fallback.
pub(super) const MAX_SPEC_DIFFS: usize = 40;

impl PkgState {
    /// The version cell: `old → new` for an update, else the version.
    pub(super) fn version_label(&self) -> String {
        match &self.from_version {
            Some(from) if *from != self.version && !self.version.is_empty() => {
                format!("{from} → {}", self.version)
            }
            _ => self.version.clone(),
        }
    }

    pub(super) fn new(version: String) -> Self {
        Self {
            version,
            from_version: None,
            status: PkgStatus::Queued,
            step: None,
            fail_reason: None,
            skipped: Vec::new(),
            log_lines: VecDeque::new(),
            log_seq: 0,
            agent_activity: VecDeque::new(),
            activity_seq: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            started: Instant::now(),
            finished: false,
            tries: 0,
            max_tries: 0,
            disk_full_detected: false,
            retry_requested: false,
            waiting_on: Vec::new(),
            sr: None,
            spec_diffs: Vec::new(),
        }
    }

    pub(super) fn add_log(&mut self, level: LogLevel, message: String) {
        self.add_log_styled(level, message, false);
    }

    /// Push a step-boundary header: bold, and preceded by a blank spacer line
    /// (skipped for the very first step) so steps are visually divided in
    /// the event log instead of blending into the lines around them.
    pub(super) fn add_step_header(&mut self, message: String) {
        if !self.log_lines.is_empty() {
            self.add_log_styled(LogLevel::Info, String::new(), false);
        }
        self.add_log_styled(LogLevel::Step, message, true);
    }

    fn add_log_styled(&mut self, level: LogLevel, message: String, emphasis: bool) {
        self.log_lines.push_back(LogLine {
            level,
            message,
            emphasis,
        });
        self.log_seq += 1;
        if self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.pop_front();
        }
    }

    pub(super) fn add_agent_activity(&mut self, kind: ActivityKind, text: String) {
        self.agent_activity.push_back(ActivityEntry { kind, text });
        self.activity_seq += 1;
        if self.agent_activity.len() > MAX_AGENT_ACTIVITY {
            self.agent_activity.pop_front();
        }
    }

    /// The Step column text: what the last step is doing or did, with its
    /// time — live while running, frozen once finished.
    pub(super) fn step_text(&self) -> String {
        let Some(step) = &self.step else {
            return "-".to_string();
        };
        let label = if step.attempt > 1 {
            format!("{} #{}", step.kind.label(), step.attempt)
        } else {
            step.kind.label().to_string()
        };
        match (&self.status, &step.done) {
            (PkgStatus::Running, _) | (PkgStatus::Between, None) => {
                format!("{label} {}", fmt_dur(step.started.elapsed()))
            }
            (PkgStatus::Failed | PkgStatus::NoSpace, done) => {
                let elapsed = done.as_ref().map_or(step.started.elapsed(), |d| d.1);
                format!("✗ {label} {}", fmt_dur(elapsed))
            }
            (PkgStatus::Between, Some((outcome, elapsed))) => {
                let marker = match outcome {
                    StepOutcome::Ok => "✓",
                    StepOutcome::Failed => "✗",
                    StepOutcome::Skipped => "⇥",
                };
                format!("{marker} {label} {}", fmt_dur(*elapsed))
            }
            _ => "-".to_string(),
        }
    }
}
