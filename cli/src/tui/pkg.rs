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
    /// Reason from `PackageOutcome::Skipped`, shown in the Step column.
    pub(super) skip_reason: Option<String>,
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
    /// The time the package has been worked on, which runs only while its
    /// status says so (see [`PkgState::advance`]).
    pub(super) clock: Clock,
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
    /// The update preparation closed the row before any build (see
    /// `Event::PackageDropped`): no build is waited for, so the run does not
    /// wait on it. A build that takes the row over clears it.
    pub(super) dropped: bool,
    /// Dependencies named by the last `PackageOutcome::MissingDeps`, sorted
    /// and deduplicated; empty once the package is retried or finishes any
    /// other way. Kept apart from the log so the Queued list can show it.
    pub(super) waiting_on: Vec<String>,
    /// The state of this package's submit request, once one was asked for.
    pub(super) sr: Option<SrMark>,
    /// The spec diffs the run recorded (`SpecChanged`), oldest first.
    pub(super) spec_diffs: Vec<String>,
    /// The `repo/arch` results the other-architecture check found failing
    /// (see `Event::ArchCheck`): a built package with any reads as a badge.
    pub(super) arch_fails: Vec<String>,
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

    /// Moves the status on for `input` (see [`PkgStatus::next`]), and the
    /// clock with it: a selection starts the package's time over, and the
    /// time runs only while the status is one that runs it.
    pub(super) fn advance(&mut self, input: Input<'_>) {
        if matches!(input, Input::Selected) {
            self.clock = Clock::default();
        }
        self.status = self.status.next(input);
        self.clock.follow(self.status.timer_runs());
    }

    /// The Time column: `-` until the package first runs, then its time,
    /// live while it runs and frozen otherwise.
    pub(super) fn time_cell(&self) -> String {
        self.clock
            .elapsed()
            .map_or_else(|| "-".to_string(), fmt_dur)
    }

    /// The status cell's text: the status, with the try for a running
    /// package once the pipeline has counted one (`running (try 2/3)`,
    /// `running (try 2/3, retrying)` after a failed step), and `done ⚠` for
    /// a built package that fails on another arch. The names of the failing
    /// arches are in the package's log, the column has no room for them.
    pub(super) fn status_cell(&self) -> String {
        let try_n = (self.max_tries > 0).then(|| format!("try {}/{}", self.tries, self.max_tries));
        match (&self.status, try_n) {
            (PkgStatus::Retrying, Some(t)) => format!("running ({t}, retrying)"),
            (PkgStatus::Running | PkgStatus::Between, Some(t)) => format!("running ({t})"),
            (PkgStatus::Succeeded, _) if !self.arch_fails.is_empty() => "done ⚠".to_string(),
            (status, _) => status.label().to_string(),
        }
    }

    /// A row the sweep reports, before its check result is applied.
    pub(super) fn checking() -> Self {
        Self {
            status: PkgStatus::Checking,
            ..Self::new(String::new())
        }
    }

    pub(super) fn new(version: String) -> Self {
        Self {
            version,
            from_version: None,
            status: PkgStatus::Queued,
            step: None,
            fail_reason: None,
            skip_reason: None,
            skipped: Vec::new(),
            log_lines: VecDeque::new(),
            log_seq: 0,
            agent_activity: VecDeque::new(),
            activity_seq: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            clock: Clock::default(),
            finished: false,
            tries: 0,
            max_tries: 0,
            disk_full_detected: false,
            retry_requested: false,
            dropped: false,
            waiting_on: Vec::new(),
            sr: None,
            spec_diffs: Vec::new(),
            arch_fails: Vec::new(),
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
    /// time — live while running, frozen once finished. A skipped package
    /// shows why it was skipped instead.
    pub(super) fn step_text(&self) -> String {
        if let (PkgStatus::Skipped | PkgStatus::CheckSkipped, Some(reason)) =
            (&self.status, &self.skip_reason)
        {
            return reason.clone();
        }
        let Some(step) = &self.step else {
            return "-".to_string();
        };
        let label = if step.attempt > 1 {
            format!("{} #{}", step.kind.label(), step.attempt)
        } else {
            step.kind.label().to_string()
        };
        match (&self.status, &step.done) {
            (PkgStatus::Running, _) | (PkgStatus::Between | PkgStatus::Retrying, None) => {
                format!("{label} {}", fmt_dur(step.started.elapsed()))
            }
            (PkgStatus::Failed | PkgStatus::NoSpace, done) => {
                let elapsed = done.as_ref().map_or(step.started.elapsed(), |d| d.1);
                format!("✗ {label} {}", fmt_dur(elapsed))
            }
            (PkgStatus::Between | PkgStatus::Retrying, Some((outcome, elapsed))) => {
                let marker = match outcome {
                    StepOutcome::Ok => "✓",
                    // The package is not finished: the pipeline is still on
                    // its tries, so the failed step is not the package's end.
                    StepOutcome::Failed => "↻",
                    StepOutcome::Skipped => "⇥",
                };
                format!("{marker} {label} {}", fmt_dur(*elapsed))
            }
            _ => "-".to_string(),
        }
    }
}
