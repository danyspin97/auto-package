//! Interactive TUI for monitoring a build run across all packages at once.
//!
//! The build loop spawns one thread per package and they all share a single
//! `Reporter`. Per-step events (`StepStarted`, `Command`, `AgentToolUse`, ...)
//! carry a `StepId` that is only unique *within* a package — two packages
//! building concurrently both start at `StepId(0)`. So attribution to a
//! package cannot be recovered from the event alone; it has to be attached
//! at the point where the package name is known. `TuiController::tap` does
//! that: the CLI asks for a tapped reporter per package thread (and one
//! "global" tap for events with no package, like `RunStarted`), and the tap
//! forwards a `(pkg, event)` pair into the render thread's channel.

use std::collections::HashMap;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event as TermEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
};

use ap_core::event::{Event, PackageOutcome, RunOutcome, SkipReason, StepKind, StepOutcome, Task};
use ap_core::sink::{MemorySink, Reporter};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_LOG_LINES: usize = 300;
const MAX_AGENT_ACTIVITY: usize = 100;

/// Bound on events queued for the render thread; beyond it events are dropped.
const EVENT_QUEUE_CAP: usize = 8192;

/// Handle used by the CLI to obtain reporters that feed the TUI.
///
/// Cheap to clone; every clone shares the same channel to the render thread.
#[derive(Clone)]
pub struct TuiController {
    tx: SyncSender<Msg>,
    /// Events dropped because the render thread's queue was full.
    dropped: std::sync::Arc<AtomicUsize>,
    /// Package names the user typed into the TUI's `n` prompt. Handed out
    /// once, via [`TuiController::take_add_requests`]; disconnects when the
    /// render thread exits, which is how the consumer learns the user quit.
    add_rx: std::sync::Arc<std::sync::Mutex<Option<Receiver<String>>>>,
}

/// Drop-based terminal restorer.
///
/// The TUI's own render thread also tears the terminal down (see [`run`]'s
/// internal guard), but it runs detached — if `main` returns while that
/// thread is still rendering, Rust terminates the thread before its teardown
/// executes, leaving raw mode and the alternate screen stuck on in the
/// user's shell.
///
/// Install one of these in `main` immediately after constructing a
/// [`TuiController`], and declare it BEFORE the controller so Rust's
/// reverse-declaration drop order runs it last — after every sender is gone
/// and the render thread has had its chance to tear down normally. Running
/// the sequence twice (here and in the render thread) is idempotent.
pub struct TerminalGuard {
    _priv: (),
}

impl TerminalGuard {
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

struct Msg {
    pkg: Option<String>,
    event: Event,
}

impl TuiController {
    /// Spawn the render thread and return a handle to feed it events.
    ///
    /// `memory` is only used for a Ctrl-C keypress inside the TUI — see
    /// `crate::handle_interrupt` for why the TUI cannot just rely on the
    /// process's real SIGINT handler firing.
    pub fn new(memory: std::sync::Arc<MemorySink>) -> io::Result<Self> {
        let (tx, rx) = sync_channel(EVENT_QUEUE_CAP);
        let (add_tx, add_rx) = channel();
        std::thread::spawn(move || {
            if let Err(e) = run(rx, add_tx, memory) {
                eprintln!("TUI error: {e}");
            }
        });
        Ok(Self {
            tx,
            dropped: std::sync::Arc::new(AtomicUsize::new(0)),
            add_rx: std::sync::Arc::new(std::sync::Mutex::new(Some(add_rx))),
        })
    }

    /// Take the receiver for package names entered at runtime (`n` key).
    /// `None` after the first call. `recv` fails once the TUI has exited.
    pub fn take_add_requests(&self) -> Option<Receiver<String>> {
        self.add_rx.lock().ok()?.take()
    }

    /// Wrap `base` so every event is still delivered to it (jsonl/memory
    /// sinks keep working exactly as without the TUI) and also forwarded to
    /// the render thread tagged with `pkg` (`None` for run-level events that
    /// don't belong to a single package).
    pub fn tap(
        &self,
        base: std::sync::Arc<dyn Reporter + Send + Sync>,
        pkg: Option<String>,
    ) -> std::sync::Arc<dyn Reporter + Send + Sync> {
        std::sync::Arc::new(Tapped {
            base,
            pkg,
            tx: self.tx.clone(),
            dropped: self.dropped.clone(),
        })
    }
}

struct Tapped {
    base: std::sync::Arc<dyn Reporter + Send + Sync>,
    pkg: Option<String>,
    tx: SyncSender<Msg>,
    dropped: std::sync::Arc<AtomicUsize>,
}

impl Reporter for Tapped {
    fn report(&self, event: &Event) {
        self.base.report(event);
        // Never block a reporter on a slow render thread: if the queue is
        // full the TUI misses this event (jsonl/memory already have it).
        match self.tx.try_send(Msg {
            pkg: self.pkg.clone(),
            event: event.clone(),
        }) {
            Err(TrySendError::Full(_)) => {
                if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
                    self.base.report(&Event::Warning {
                        step: None,
                        message: "TUI cannot keep up; dropping events from its view \
                                  (the run log is complete)"
                            .to_string(),
                    });
                }
            }
            Err(TrySendError::Disconnected(_)) | Ok(()) => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogLevel {
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
    fn color(self) -> Color {
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
struct LogLine {
    level: LogLevel,
    message: String,
    /// Rendered bold — used for step-boundary headers so they stand out from
    /// the regular log lines between them.
    emphasis: bool,
}

#[derive(Clone, PartialEq, Eq)]
enum PkgStatus {
    Queued,
    Running,
    Succeeded,
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
}

impl PkgStatus {
    fn label(&self) -> &'static str {
        match self {
            PkgStatus::Queued => "queued",
            PkgStatus::Running => "running",
            PkgStatus::Between => "working",
            PkgStatus::Succeeded => "done",
            PkgStatus::AlreadyBuilt => "cached",
            PkgStatus::MissingDeps => "needs deps",
            PkgStatus::Failed => "failed",
            PkgStatus::NoSpace => "no space",
        }
    }

    fn color(&self) -> Color {
        match self {
            PkgStatus::Queued => Color::DarkGray,
            PkgStatus::Running | PkgStatus::Between => Color::Yellow,
            PkgStatus::Succeeded | PkgStatus::AlreadyBuilt => Color::Green,
            PkgStatus::MissingDeps => Color::Magenta,
            PkgStatus::Failed => Color::Red,
            PkgStatus::NoSpace => Color::LightRed,
        }
    }
}

/// Whether `text` names the classic ENOSPC condition — glibc's `strerror`
/// text for it, verbatim, is what `tar`, `cp`, and `rpmbuild` all actually
/// print, so this is the literal signature to look for rather than an errno
/// number that never reaches the log as such. Matched case-insensitively:
/// seen both as `No space left on device` and, inside some tools' own
/// wrapping, lowercased.
fn looks_like_disk_full(text: &str) -> bool {
    text.to_ascii_lowercase()
        .contains("no space left on device")
}

/// Longest prefix of `s` whose display width fits in `max_width` columns.
/// Zero-width characters (combining marks, variation selectors) and the
/// character after a zero-width joiner stay with the character before them,
/// so the cut never splits such a cluster.
fn prefix_within_width(s: &str, max_width: usize) -> &str {
    let mut used = 0;
    let mut end = 0;
    let mut after_zwj = false;
    for (idx, c) in s.char_indices() {
        let width = c.width().unwrap_or(0);
        if width > 0 && !after_zwj {
            // `idx` starts a new cluster, so `s[..idx]` is a candidate cut.
            if used > max_width {
                return &s[..end];
            }
            end = idx;
        }
        used += width;
        after_zwj = c == '\u{200d}';
    }
    if used <= max_width { s } else { &s[..end] }
}

/// Shortens `s` to at most `max_width` display columns, ending in "..." when cut.
fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        s.to_string()
    } else {
        format!("{}...", prefix_within_width(s, max_width.saturating_sub(3)))
    }
}

/// Greedy word wrap to `width` display columns. Newlines in `text` stay line
/// breaks and blank lines stay blank. A word wider than `width` is broken
/// where it must be, and a glyph wider than the whole line still gets a line.
/// Returns the wrapped lines, so their count is what a viewport scrolls over.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut current = String::new();
        let mut current_w = 0;
        for word in paragraph.split_whitespace() {
            let word_w = word.width();
            if current_w > 0 && current_w + 1 + word_w <= width {
                current.push(' ');
                current.push_str(word);
                current_w += 1 + word_w;
                continue;
            }
            if current_w > 0 {
                lines.push(std::mem::take(&mut current));
            }
            let mut rest = word;
            while rest.width() > width {
                let mut head = prefix_within_width(rest, width);
                if head.is_empty() {
                    head = &rest[..rest.chars().next().map_or(0, char::len_utf8)];
                }
                lines.push(head.to_string());
                rest = &rest[head.len()..];
            }
            current.push_str(rest);
            current_w = rest.width();
        }
        // A paragraph whose last glyph was broken off whole leaves nothing
        // behind; only a blank paragraph should still produce its empty line.
        if !current.is_empty() || paragraph.trim().is_empty() {
            lines.push(current);
        }
    }
    lines
}

/// What kind of thing an [`ActivityEntry`] records — lets the Agent Activity
/// pane style each kind differently instead of a single uniform color, since
/// a turn's start, its tool calls, and its final decision read very
/// differently in a wall of otherwise-identical text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    /// A new agent turn starting (`Event::AgentPrompt`).
    Task,
    /// A tool call the agent made mid-turn (`Event::AgentToolUse`).
    ToolUse,
    /// The agent's final decision for the turn (`Event::AgentReply`).
    Reply,
}

impl ActivityKind {
    fn label(self) -> &'static str {
        match self {
            ActivityKind::Task => "Task",
            ActivityKind::ToolUse => "Tool",
            ActivityKind::Reply => "Reply",
        }
    }

    fn color(self) -> Color {
        match self {
            ActivityKind::Task => Color::Cyan,
            ActivityKind::ToolUse => Color::Blue,
            ActivityKind::Reply => Color::Green,
        }
    }
}

#[derive(Clone)]
struct ActivityEntry {
    kind: ActivityKind,
    text: String,
}

/// One step of a package's build, from `StepStarted` until it is finished.
struct StepInfo {
    kind: StepKind,
    /// 1-based start count of this kind; a retry shows as `#2`.
    attempt: u32,
    /// Iteration of the validate/build loop the step ran in.
    round: u32,
    started: Instant,
    /// Outcome and the event's own elapsed time, set by `StepFinished` (or by
    /// `PackageFinished` if the worker died with the step still open).
    done: Option<(StepOutcome, Duration)>,
}

struct PkgState {
    version: String,
    status: PkgStatus,
    /// The last step seen, never cleared between steps; cleared only for a
    /// fresh attempt and for packages that finished without failing.
    step: Option<StepInfo>,
    /// Reason from `PackageOutcome::Failed`, shown in the detail pane.
    fail_reason: Option<String>,
    /// Conditional steps that did not run, in the order they were skipped.
    skipped: Vec<(StepKind, SkipReason)>,
    log_lines: Vec<LogLine>,
    /// Lines ever added, including ones since evicted from `log_lines`. Only
    /// grows, so a view can count arrivals even once the cap is hit.
    log_seq: u64,
    agent_activity: Vec<ActivityEntry>,
    /// Same as `log_seq`, for `agent_activity`.
    #[cfg_attr(not(test), allow(dead_code))]
    activity_seq: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    started: Instant,
    finished: bool,
    /// Current/max agent fix-attempt count within this `run_package` call
    /// (`Task::FixAfterBuild`'s `attempt`/`max_attempts`). Stays `0`/`0`
    /// until the first post-build fix attempt — a package fixed on the
    /// first try, or still on its initial build, never needed one.
    tries: u32,
    max_tries: u32,
    /// Set once this package's build log has shown [`looks_like_disk_full`]'s
    /// signature. Sticky for the rest of the run: an `ExtractResult` only
    /// arrives for the attempt that just failed, but the eventual
    /// `PackageFinished` that turns this into a visible `PkgStatus::NoSpace`
    /// arrives later, after the agent's (likely futile) fix attempt — so
    /// this has to survive until then rather than being read inline.
    disk_full_detected: bool,
    /// The user pressed `r` and the root request was sent; further presses
    /// are ignored until the package's status changes, so holding or
    /// repeating the key doesn't queue duplicate builds.
    retry_requested: bool,
    /// Dependencies named by the last `PackageOutcome::MissingDeps`, sorted
    /// and deduplicated; empty once the package is retried or finishes any
    /// other way. Kept apart from the log so the Queued list can show it.
    waiting_on: Vec<String>,
}

impl PkgState {
    fn new(version: String) -> Self {
        Self {
            version,
            status: PkgStatus::Queued,
            step: None,
            fail_reason: None,
            skipped: Vec::new(),
            log_lines: Vec::new(),
            log_seq: 0,
            agent_activity: Vec::new(),
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
        }
    }

    fn add_log(&mut self, level: LogLevel, message: String) {
        self.add_log_styled(level, message, false);
    }

    /// Push a step-boundary header: bold, and preceded by a blank spacer line
    /// (skipped for the very first step) so steps are visually divided in
    /// the event log instead of blending into the lines around them.
    fn add_step_header(&mut self, message: String) {
        if !self.log_lines.is_empty() {
            self.add_log_styled(LogLevel::Info, String::new(), false);
        }
        self.add_log_styled(LogLevel::Step, message, true);
    }

    fn add_log_styled(&mut self, level: LogLevel, message: String, emphasis: bool) {
        self.log_lines.push(LogLine {
            level,
            message,
            emphasis,
        });
        self.log_seq += 1;
        if self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.remove(0);
        }
    }

    fn add_agent_activity(&mut self, kind: ActivityKind, text: String) {
        self.agent_activity.push(ActivityEntry { kind, text });
        self.activity_seq += 1;
        if self.agent_activity.len() > MAX_AGENT_ACTIVITY {
            self.agent_activity.remove(0);
        }
    }

    /// The Step column text: what the last step is doing or did, with its
    /// time — live while running, frozen once finished.
    fn step_text(&self) -> String {
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

/// Compact duration for the Step column: `42s`, `1m23s`.
fn fmt_dur(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

struct TuiState {
    order: Vec<String>,
    packages: HashMap<String, PkgState>,
    // Only the most recent run-level message is ever shown (the footer),
    // so there is nothing to gain from keeping a bounded history here the
    // way per-package logs do.
    last_global_log: Option<LogLine>,
    run_outcome: Option<RunOutcome>,
    run_start: Option<Instant>,
    /// Toggled with `a` — show every package, including ones `visible_order`
    /// would otherwise hide once they succeed.
    show_all: bool,
    /// Full pip-closure size from the most recent `ResolveFinished` — there
    /// is no live equivalent (see that event's handler), so this is kept as
    /// a point-in-time value rather than derived every frame like `built`.
    resolve_total: Option<usize>,
    /// Same snapshot caveat as `resolve_total`: how many of the closure the
    /// distro already satisfies, as of the last resolve pass.
    resolve_in_distro: Option<usize>,
}

impl TuiState {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            packages: HashMap::new(),
            last_global_log: None,
            run_outcome: None,
            resolve_total: None,
            resolve_in_distro: None,
            run_start: None,
            show_all: false,
        }
    }

    /// Total cost across every package tracked so far. Derived from the
    /// per-package totals (updated live on every `AgentReply`) rather than
    /// cached separately, so the header reflects spend in real time instead
    /// of only jumping to the right number once `RunFinished` arrives.
    fn total_cost_usd(&self) -> f64 {
        self.packages.values().map(|p| p.cost_usd).sum()
    }

    fn add_global_log(&mut self, level: LogLevel, message: String) {
        self.last_global_log = Some(LogLine {
            level,
            message,
            emphasis: false,
        });
    }

    /// Packages still worth showing in the table: everything except ones that
    /// built fine and need no further attention. Failed and missing-deps
    /// packages stay visible since they may still need attention or get
    /// retried; the header's `n/N packages done` count still includes the
    /// hidden ones (it is derived from `packages`, not this list). Hiding
    /// them is only the default — `show_all` (toggled with `a`) lets you
    /// spot-check a specific package that already succeeded instead of
    /// having to trust the aggregate count with no way to see which ones.
    fn visible_order(&self) -> Vec<&String> {
        self.order
            .iter()
            .filter(|name| {
                self.show_all
                    || !matches!(
                        self.packages[*name].status,
                        PkgStatus::Succeeded | PkgStatus::AlreadyBuilt
                    )
            })
            .collect()
    }

    fn pkg_mut(&mut self, name: &str) -> &mut PkgState {
        if !self.packages.contains_key(name) {
            self.order.push(name.to_string());
            self.packages
                .insert(name.to_string(), PkgState::new(String::new()));
        }
        self.packages.get_mut(name).unwrap()
    }

    fn apply(&mut self, msg: Msg) {
        // Events that name their own package (TargetSelected, PackageFinished)
        // are routed by that name regardless of which tap carried them, since
        // they are reported once from the main thread rather than from
        // inside a per-package worker.
        match &msg.event {
            Event::RunStarted { .. } => {
                self.run_start = Some(Instant::now());
                self.add_global_log(LogLevel::Info, "Run started".to_string());
            }
            Event::RunFinished {
                outcome,
                elapsed,
                usage,
            } => {
                self.run_outcome = Some(outcome.clone());
                let level = match outcome {
                    RunOutcome::Success => LogLevel::Success,
                    _ => LogLevel::Error,
                };
                self.add_global_log(
                    level,
                    format!(
                        "Run finished: {outcome} in {:.1}s (${:.4})",
                        elapsed.as_secs_f64(),
                        usage.cost_usd
                    ),
                );
            }
            Event::ResolveStarted { root } => {
                self.add_global_log(LogLevel::Info, format!("Resolving {root}"));
            }
            Event::ResolveFinished {
                total,
                built,
                in_distro,
                remaining,
            } => {
                // `built`/`remaining` here are a snapshot for this one
                // resolve pass only — re-resolving the pip closure is rare
                // (only the current batch's root re-runs `select_target`;
                // the far more frequent ad hoc dependency builds spawned
                // from `UnresolvableDeps`/`MissingDeps` never do), so the
                // header's own `built` figure is computed live from
                // `self.packages` instead (see `render_summary`) rather
                // than trusted from this message — otherwise it reads as
                // stuck at whatever this first snapshot said for most of a
                // long run, long after packages have visibly succeeded in
                // the table. `total`/`in_distro` have no live equivalent
                // (a distro-satisfied dependency is never selected as a
                // target, so it never gets a row here at all) and are kept.
                self.resolve_total = Some(*total);
                self.resolve_in_distro = Some(*in_distro);
                self.add_global_log(
                    LogLevel::Info,
                    format!(
                        "Resolved: {total} total, {built} built, {in_distro} in the distro, {remaining} remaining"
                    ),
                );
            }
            Event::TargetSelected { pkg, version, .. } => {
                let state = self.pkg_mut(pkg);
                state.version = version.clone();
                state.status = PkgStatus::Queued;
                state.started = Instant::now();
                state.step = None;
                state.fail_reason = None;
                state.skipped.clear();
                // A fresh attempt — the condition that caused last time's
                // `NoSpace` may already be gone (that's the whole point of
                // the `r` retry keybinding), so don't carry the flag over
                // and have it immediately reclassify a *new* failure.
                state.disk_full_detected = false;
                // The retry (if that's what this is) has been picked up, and
                // the package is running again: it must not count as done in
                // the header or show the previous attempt's tries.
                state.retry_requested = false;
                state.finished = false;
                state.tries = 0;
                state.max_tries = 0;
                // The package is running again, so it no longer waits on
                // what the previous attempt reported.
                state.waiting_on.clear();
            }
            Event::PackageFinished {
                pkg,
                version,
                outcome,
            } => {
                let state = self.pkg_mut(pkg);
                if state.version.is_empty() {
                    state.version = version.clone();
                }
                state.finished = true;
                state.retry_requested = false;
                state.waiting_on = match outcome {
                    PackageOutcome::MissingDeps { deps } => {
                        let mut deps = deps.clone();
                        deps.sort();
                        deps.dedup();
                        deps
                    }
                    _ => Vec::new(),
                };
                state.status = match outcome {
                    PackageOutcome::Built => PkgStatus::Succeeded,
                    PackageOutcome::AlreadyBuilt => PkgStatus::AlreadyBuilt,
                    PackageOutcome::MissingDeps { .. } => PkgStatus::MissingDeps,
                    PackageOutcome::Failed { reason } => {
                        if state.disk_full_detected || looks_like_disk_full(reason) {
                            PkgStatus::NoSpace
                        } else {
                            PkgStatus::Failed
                        }
                    }
                };
                match outcome {
                    PackageOutcome::Failed { reason } => {
                        // Keep the failed step so the Step column can name it.
                        // A worker that died mid-step never sent `StepFinished`,
                        // so close the step here.
                        if let Some(step) = state.step.as_mut().filter(|s| s.done.is_none()) {
                            let elapsed = step.started.elapsed();
                            step.done = Some((StepOutcome::Failed, elapsed));
                        }
                        state.fail_reason = Some(reason.clone());
                    }
                    PackageOutcome::Built
                    | PackageOutcome::AlreadyBuilt
                    | PackageOutcome::MissingDeps { .. } => {
                        state.step = None;
                        state.fail_reason = None;
                    }
                }
                let msg = match outcome {
                    PackageOutcome::Built => "built".to_string(),
                    PackageOutcome::AlreadyBuilt => "already built".to_string(),
                    PackageOutcome::MissingDeps { deps } => {
                        format!("needs: {}", deps.join(", "))
                    }
                    PackageOutcome::Failed { reason } => format!("failed: {reason}"),
                };
                state.add_log(
                    match outcome {
                        PackageOutcome::Failed { .. } => LogLevel::Error,
                        PackageOutcome::MissingDeps { .. } => LogLevel::Warn,
                        _ => LogLevel::Success,
                    },
                    msg,
                );
            }
            // Everything below only makes sense scoped to a package; drop it
            // if it somehow arrives untagged.
            _ => {
                let Some(pkg) = &msg.pkg else { return };
                self.apply_pkg_event(pkg, &msg.event);
            }
        }
    }

    fn apply_pkg_event(&mut self, pkg: &str, event: &Event) {
        let state = self.pkg_mut(pkg);
        match event {
            Event::StepStarted {
                kind,
                attempt,
                round,
                ..
            } => {
                // The flag belongs to the failure that hit ENOSPC. Only the
                // failure-analysis and agent steps that follow that build
                // may still report it; any other step starts new work whose
                // failure must not be blamed on the disk.
                if !kind.spec().keeps_disk_flag {
                    state.disk_full_detected = false;
                }
                // The notes turn only runs after a successful build, so the package
                // is already done; showing it as running would hide that.
                state.status = if *kind == StepKind::PackageNotes {
                    PkgStatus::Succeeded
                } else {
                    PkgStatus::Running
                };
                state.step = Some(StepInfo {
                    kind: *kind,
                    attempt: *attempt,
                    round: *round,
                    started: Instant::now(),
                    done: None,
                });
                let header = if *attempt > 1 {
                    format!("▸ {} #{attempt}", kind.label())
                } else {
                    format!("▸ {}", kind.label())
                };
                state.add_step_header(header);
            }
            // Never a step of its own: the current step stays on screen.
            Event::StepSkipped { kind, reason } => {
                state.skipped.push((*kind, reason.clone()));
                state.add_log(
                    LogLevel::Warn,
                    format!("⇥ Skipped {}: {reason}", kind.label()),
                );
            }
            Event::StepFinished {
                outcome, elapsed, ..
            } => {
                let (level, marker) = match outcome {
                    StepOutcome::Ok => (LogLevel::Success, "✓"),
                    StepOutcome::Failed => (LogLevel::Error, "✗"),
                    StepOutcome::Skipped => (LogLevel::Warn, "⇥"),
                };
                state.add_log(
                    level,
                    format!("  {marker} {outcome} in {:.2}s", elapsed.as_secs_f64()),
                );
                // Between steps (or before the agent turn / PackageFinished):
                // the step stays on screen with its final time.
                if let Some(step) = state.step.as_mut() {
                    step.done = Some((outcome.clone(), *elapsed));
                    // A failed agent turn is a different failure than the
                    // ENOSPC build that preceded it.
                    if step.kind.spec().keeps_disk_flag && matches!(outcome, StepOutcome::Failed) {
                        state.disk_full_detected = false;
                    }
                }
                // The notes turn is the last thing a successful package does: once it
                // ends, the row is done and must not keep showing the notes step
                // (a missing PackageFinished would otherwise leave that text).
                if state
                    .step
                    .as_ref()
                    .is_some_and(|s| s.kind == StepKind::PackageNotes)
                {
                    state.step = None;
                    state.status = PkgStatus::Succeeded;
                } else {
                    state.status = PkgStatus::Between;
                }
            }
            Event::BuildFailed { classification, .. } => {
                state.add_log(LogLevel::Error, format!("Build failed: {classification:?}"));
            }
            Event::ExtractResult { extract, .. } => {
                if extract.error_lines.iter().any(|l| looks_like_disk_full(l))
                    || looks_like_disk_full(&extract.excerpt)
                {
                    state.disk_full_detected = true;
                }
                let phase = extract.phase.as_deref().unwrap_or("unknown phase");
                let mut msg = format!("  ↳ failure located in {phase}");
                if let Some(genre) = &extract.genre {
                    msg.push_str(&format!(" ({genre})"));
                }
                if !extract.has_signal {
                    msg.push_str(" — no clear signal, showing log tail");
                }
                state.add_log(LogLevel::Warn, msg);
            }
            Event::CommandDone { code, elapsed, .. } => {
                let level = if *code == 0 {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                };
                state.add_log(
                    level,
                    format!("  = exit {code} ({:.2}s)", elapsed.as_secs_f64()),
                );
            }
            Event::AgentPrompt { task, .. } => {
                let task_str = match task {
                    Task::Review => "Reviewing spec".to_string(),
                    Task::FixAfterBuild {
                        attempt,
                        max_attempts,
                    } => {
                        state.tries = *attempt;
                        state.max_tries = *max_attempts;
                        format!("Fixing spec (attempt {attempt}/{max_attempts})")
                    }
                    Task::TranslateUnresolvableDeps => "Looking up dependency names".to_string(),
                };
                state.add_agent_activity(ActivityKind::Task, task_str.clone());
                state.add_log(LogLevel::Info, format!("Agent: {task_str}"));
            }
            Event::NotesUsage { usage, .. } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
            }
            Event::AgentReply {
                response, usage, ..
            } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
                // The reasoning goes in the entry too, after a line break, so
                // Enter can show the whole decision; the list shows only the
                // first line.
                state.add_agent_activity(
                    ActivityKind::Reply,
                    format!(
                        "{} (confidence: {}, ${:.4})\n{}",
                        response.action,
                        response.confidence.as_deref().unwrap_or("?"),
                        usage.cost_usd,
                        response.reasoning
                    ),
                );
                let reasoning = truncate_with_ellipsis(&response.reasoning, 80);
                state.add_log(
                    LogLevel::Info,
                    format!("Agent {}: {reasoning}", response.action),
                );
            }
            Event::AgentToolUse { tool, input, .. } => {
                state.add_agent_activity(ActivityKind::ToolUse, format!("→ {tool} {input}"));
            }
            Event::Progress { message, .. } => {
                state.add_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } => {
                state.add_log(LogLevel::Warn, message.clone());
            }
            Event::MissingPackages { names, .. } => {
                state.add_log(
                    LogLevel::Warn,
                    format!("Missing packages: {}", names.join(", ")),
                );
            }
            Event::SpecChanged {
                lines_added,
                lines_removed,
                ..
            } => {
                if *lines_added > 0 || *lines_removed > 0 {
                    state.add_log(
                        LogLevel::Success,
                        format!("Spec changed: +{lines_added} -{lines_removed}"),
                    );
                }
            }
            Event::SpecRejected { reason, .. } => {
                state.add_log(LogLevel::Warn, format!("Spec change rejected: {reason:?}"));
            }
            Event::Command { program, argv, .. } => {
                let cmd = format!("{program} {}", argv.join(" "));
                state.add_log(
                    LogLevel::Info,
                    format!("$ {}", truncate_with_ellipsis(&cmd, 100)),
                );
            }
            Event::Artifact { name, .. } => {
                state.add_log(LogLevel::Success, format!("Artifact: {name}"));
            }
            _ => {}
        }
    }
}

fn run(
    rx: Receiver<Msg>,
    add_tx: Sender<String>,
    memory: std::sync::Arc<MemorySink>,
) -> io::Result<()> {
    // Installed before the fallible calls: `enable_raw_mode` succeeding and
    // `EnterAlternateScreen` then failing must still restore the terminal.
    // (Teardown is idempotent, so a guard after a failed first call is fine.)
    let _terminal_guard = TerminalGuard::new();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // However the event loop ends — clean exit, a render/input error, or a
    // panic unwinding through here — raw mode and the alternate screen must
    // be torn down. Left engaged, every line printed afterwards (including
    // the error itself) lands inside the broken terminal state instead of
    // the user's normal screen. The guard restores both on ANY exit path
    // from this function. (Ctrl-C is the one exit path that does NOT come
    // back through here at all — `crate::handle_interrupt` does its own
    // equivalent teardown and then ends the process on the spot.)
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    // Without this, the alternate screen keeps whatever happened to be in its
    // buffer when we switched to it (shell scrollback on terminals that mirror
    // the primary buffer, previous-run leftovers on terminals that preserve
    // the alt buffer across invocations). Ratatui's `draw` only sends diffs
    // against its own previous buffer — not the terminal's — so cells we
    // render as default (space + default style) silently keep that leftover
    // content showing through. One hard clear up front wipes the slate.
    terminal.clear()?;

    let result = event_loop(&mut terminal, rx, &add_tx, &memory);
    let _ = terminal.show_cursor();
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: Receiver<Msg>,
    add_tx: &Sender<String>,
    memory: &MemorySink,
) -> io::Result<()> {
    let mut state = TuiState::new();
    // `Some` while the "add package" prompt is open and capturing keys.
    let mut input: Option<String> = None;
    let mut table_state = TableState::default();
    // Tracked by package name rather than raw table index: `visible_order()`
    // shrinks whenever a package finishes, which would otherwise shift every
    // later row up and silently hand the highlight to a different package.
    let mut selected: Option<String> = None;
    // Scroll state for the selected package. It belongs to `browse_for`, the
    // package it was last synced to: a key press can change `selected` before
    // the next frame, so comparing against the previous frame's selection
    // would miss the change.
    let mut browse = BrowseState::default();
    let mut browse_for: Option<String> = None;
    let tick_rate = Duration::from_millis(150);
    let mut finished_at: Option<Instant> = None;
    let mut senders_dropped = false;

    loop {
        // Drain everything currently queued so a burst of events (e.g. many
        // packages starting at once) renders as one frame, not one per event.
        loop {
            match rx.try_recv() {
                Ok(msg) => state.apply(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    senders_dropped = true;
                    finished_at.get_or_insert(Instant::now());
                    break;
                }
            }
        }

        {
            let visible = state.visible_order();
            if selected
                .as_deref()
                .is_none_or(|s| !visible.iter().any(|n| n.as_str() == s))
            {
                selected = visible.first().map(|n| (*n).clone());
            }
            table_state.select(
                selected
                    .as_ref()
                    .and_then(|s| visible.iter().position(|n| n.as_str() == s)),
            );
        }
        // Lines that arrived since the last frame push a scrolled view up so
        // it keeps showing the same content; a view at the tail follows.
        let log_seq = selected
            .as_deref()
            .and_then(|s| state.packages.get(s))
            .map_or(0, |p| p.log_seq);
        if selected != browse_for {
            browse.reset_for_package(log_seq);
            browse_for = selected.clone();
        } else {
            browse.log_off = follow_adjust(browse.log_off, log_seq, browse.log_seen);
            browse.log_seen = log_seq;
        }

        let mut feedback = RenderFeedback::default();
        terminal.draw(|f| {
            feedback = render(f, &state, &mut table_state, input.as_deref(), &browse);
        })?;
        // Keep the offset within what rendering can show, or PageDown
        // appears dead after holding PageUp at the top.
        browse.log_off = feedback.log_off;
        browse.act_top = feedback.act_top;
        browse.detail_off = feedback.detail_off;

        if event::poll(tick_rate)?
            && let TermEvent::Key(key) = event::read()?
        {
            // The prompt swallows every key except Ctrl-C, so typing a name
            // containing `q`, `j`, `a`... doesn't quit or move the selection.
            if let Some(buf) = input.as_mut()
                && !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL))
            {
                match prompt_key(buf, key.code) {
                    PromptAction::Pending => {}
                    PromptAction::Cancel => input = None,
                    PromptAction::Submit(name) => {
                        input = None;
                        if add_tx.send(name.clone()).is_ok() {
                            state.add_global_log(
                                LogLevel::Info,
                                format!("Queued {name} as a new root package"),
                            );
                        } else {
                            state.add_global_log(
                                LogLevel::Error,
                                format!(
                                    "Cannot add {name}: the run is no longer accepting packages"
                                ),
                            );
                        }
                    }
                }
                continue;
            }
            // The `/` prompt captures keys the same way the add prompt does.
            if let Some(buf) = browse.filter_input.as_mut()
                && !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL))
            {
                match prompt_key(buf, key.code) {
                    PromptAction::Pending => {}
                    PromptAction::Submit(text) => {
                        browse.filter_input = None;
                        browse.filter = Some(text);
                        browse.log_off = 0;
                    }
                    PromptAction::Cancel => {
                        browse.filter_input = None;
                        // A blank Enter clears the filter; Esc leaves it be.
                        if key.code == KeyCode::Enter {
                            browse.filter = None;
                            browse.log_off = 0;
                        }
                    }
                }
                continue;
            }
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('+') => input = Some(String::new()),
                KeyCode::Char('/') if browse.focus == Focus::Log => {
                    browse.filter_input = Some(browse.filter.clone().unwrap_or_default());
                }
                KeyCode::Esc if esc_action(&browse) != EscAction::Quit => match esc_action(&browse)
                {
                    EscAction::ClosePrompt => browse.filter_input = None,
                    EscAction::ClearFilter => {
                        browse.filter = None;
                        browse.log_off = 0;
                    }
                    EscAction::Collapse => browse.act_expanded = false,
                    EscAction::Quit => {}
                },
                // Quit means quit: end the whole process (killing the running
                // builds), not just the UI. Detaching used to leave the
                // pipeline running headless with nothing on screen and no
                // way to stop it short of `kill`.
                // After a successful run there is nothing to interrupt: leave
                // normally so the process exits 0.
                KeyCode::Char('q') | KeyCode::Esc => {
                    if matches!(state.run_outcome, Some(RunOutcome::Success)) {
                        break;
                    }
                    crate::handle_interrupt(true, memory)
                }
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    if matches!(state.run_outcome, Some(RunOutcome::Success)) {
                        break;
                    }
                    // Raw mode disables signal generation entirely, so the OS
                    // never delivers a real SIGINT while the TUI is running —
                    // this keypress IS the only place Ctrl-C shows up. Same
                    // shutdown as `q`/Esc above.
                    crate::handle_interrupt(true, memory);
                }
                KeyCode::Tab => browse.focus = cycle_focus(browse.focus, false),
                KeyCode::BackTab => browse.focus = cycle_focus(browse.focus, true),
                // In the table, the arrows move the package selection; in the
                // log pane they scroll it. The Activity pane has no keys yet.
                KeyCode::Down | KeyCode::Char('j') if browse.focus == Focus::Table => {
                    select_next(&mut selected, &state)
                }
                KeyCode::Up | KeyCode::Char('k') if browse.focus == Focus::Table => {
                    select_prev(&mut selected, &state)
                }
                KeyCode::Char('a') => state.show_all = !state.show_all,
                // Works in every focus: the lists replace the detail panes
                // wholesale, so there is no pane the key could belong to.
                KeyCode::Char('l') => browse.show_lists = !browse.show_lists,
                // The lists have no cursor, so they scroll from the top like
                // the expanded activity text, on the keys the log pane uses.
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                    if browse.show_lists && browse.focus == Focus::Log =>
                {
                    browse.lists_off = text_scroll_key(
                        key.code,
                        browse.lists_off,
                        feedback.lists_total,
                        feedback.lists_viewport,
                    );
                }
                // Retry a package that failed with "No space left on
                // device" (`PkgStatus::NoSpace`) once the user has freed
                // some — re-sent through the same channel as the `n` add
                // prompt, since from the build loop's side a retry and a
                // freshly typed package name are the same request. Any
                // other status is a no-op: there is nothing a disk cleanup
                // would fix for a real spec defect.
                KeyCode::Char('r') => {
                    if let Some(name) = selected.as_ref()
                        && state
                            .packages
                            .get(name.as_str())
                            .is_some_and(|p| p.status == PkgStatus::NoSpace && !p.retry_requested)
                    {
                        if add_tx.send(name.clone()).is_ok() {
                            if let Some(p) = state.packages.get_mut(name.as_str()) {
                                p.retry_requested = true;
                            }
                            state.add_global_log(
                                LogLevel::Info,
                                format!("Retrying {name} (no space left on device)"),
                            );
                        } else {
                            state.add_global_log(
                                LogLevel::Error,
                                format!(
                                    "Cannot retry {name}: the run is no longer accepting packages"
                                ),
                            );
                        }
                    }
                }
                // Event Log scrollback: pin to the live tail by default (0),
                // step back through history on demand without losing new
                // lines arriving underneath — they just wait below the
                // viewport until `End` jumps back to them.
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                    if browse.focus == Focus::Activity =>
                {
                    if browse.act_expanded {
                        browse.detail_off = text_scroll_key(
                            key.code,
                            browse.detail_off,
                            feedback.detail_total,
                            feedback.detail_viewport,
                        );
                    } else {
                        let sel = browse.act_sel.min(feedback.act_len.saturating_sub(1));
                        browse.act_sel =
                            activity_move(sel, feedback.act_len, key.code, feedback.act_viewport);
                    }
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    browse.log_off = scroll_key(
                        key.code,
                        browse.log_off,
                        feedback.log_total,
                        feedback.log_viewport,
                    );
                }
                KeyCode::Enter if browse.focus == Focus::Activity && feedback.act_len > 0 => {
                    // Freeze the selection on the entry being read: while it
                    // follows the newest entry, the text would change underneath.
                    browse.act_sel = browse.act_sel.min(feedback.act_len - 1);
                    browse.act_expanded = !browse.act_expanded;
                    browse.detail_off = 0;
                }
                _ => {}
            }
        }

        if state.run_outcome.is_some() {
            let at = finished_at.get_or_insert(Instant::now());
            if at.elapsed() >= Duration::from_secs(2) {
                break;
            }
        } else if senders_dropped {
            // All senders are gone but no `RunFinished` ever arrived (early
            // abort before the run was announced, or the main thread
            // dropped its reporters). No more events can ever arrive — spin
            // only to let input finish and then exit so the terminal
            // teardown guard runs.
            break;
        }
    }

    Ok(())
}

enum PromptAction {
    Pending,
    Cancel,
    Submit(String),
}

/// Apply one keypress to the "add package" prompt buffer.
fn prompt_key(buf: &mut String, code: KeyCode) -> PromptAction {
    match code {
        KeyCode::Esc => PromptAction::Cancel,
        KeyCode::Enter => {
            let name = buf.trim().to_string();
            if name.is_empty() {
                PromptAction::Cancel
            } else {
                PromptAction::Submit(name)
            }
        }
        KeyCode::Backspace => {
            buf.pop();
            PromptAction::Pending
        }
        KeyCode::Char(c) => {
            buf.push(c);
            PromptAction::Pending
        }
        _ => PromptAction::Pending,
    }
}

fn select_next(selected: &mut Option<String>, state: &TuiState) {
    let visible = state.visible_order();
    if visible.is_empty() {
        *selected = None;
        return;
    }
    let i = selected
        .as_deref()
        .and_then(|s| visible.iter().position(|n| n.as_str() == s))
        .unwrap_or(0);
    *selected = Some(visible[(i + 1).min(visible.len() - 1)].clone());
}

fn select_prev(selected: &mut Option<String>, state: &TuiState) {
    let visible = state.visible_order();
    if visible.is_empty() {
        *selected = None;
        return;
    }
    let i = selected
        .as_deref()
        .and_then(|s| visible.iter().position(|n| n.as_str() == s))
        .unwrap_or(0);
    *selected = Some(visible[i.saturating_sub(1)].clone());
}

/// Which pane the arrow keys and paging act on. Tab cycles it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Focus {
    #[default]
    Table,
    Log,
    Activity,
}

/// Scroll state for the selected package's panes, kept by the event loop
/// across frames. Offsets are clamped against the previous frame's sizes.
#[derive(Default)]
struct BrowseState {
    focus: Focus,
    /// Lines above the tail of the Event Log; 0 follows the newest line.
    log_off: usize,
    /// `PkgState::log_seq` at the last frame, to count the lines that arrived.
    log_seen: u64,
    /// Selected Agent Activity entry, an index into the package's list.
    /// `usize::MAX` means the newest entry, so the selection keeps following
    /// as entries arrive until the user moves it.
    act_sel: usize,
    /// First Agent Activity entry shown in the list.
    act_top: usize,
    /// Enter shows the selected entry's full text in place of the Event Log.
    act_expanded: bool,
    /// First wrapped line shown of the expanded text.
    detail_off: usize,
    /// Applied Event Log filter: only lines containing it are shown.
    filter: Option<String>,
    /// The `/` prompt while it is open, with the text typed so far.
    filter_input: Option<String>,
    /// `l` replaces the detail panes with the Queued and Built lists.
    show_lists: bool,
    /// First line shown of the lists, shared by both so they scroll together.
    lists_off: usize,
}

impl BrowseState {
    /// Start over for a newly selected package: another package's offsets
    /// mean nothing here.
    fn reset_for_package(&mut self, log_seq: u64) {
        self.log_off = 0;
        self.log_seen = log_seq;
        self.act_sel = usize::MAX;
        self.act_top = 0;
        self.act_expanded = false;
        self.detail_off = 0;
        self.filter = None;
        self.filter_input = None;
    }
}

/// What one frame learned about the pane sizes, handed back to the event loop
/// so key handling clamps against what is actually on screen.
#[derive(Default, Clone, Copy)]
struct RenderFeedback {
    log_total: usize,
    log_viewport: usize,
    /// The log offset after clamping to this frame's sizes.
    log_off: usize,
    act_len: usize,
    act_viewport: usize,
    /// The Agent Activity top after keeping the selection on screen.
    act_top: usize,
    /// Wrapped lines of the expanded text, and how many of them fit.
    detail_total: usize,
    detail_viewport: usize,
    detail_off: usize,
    /// Longest of the two lists and how many lines fit, for scrolling them.
    lists_total: usize,
    lists_viewport: usize,
}

/// The pane after (or, with `back`, before) `f`, wrapping around.
fn cycle_focus(f: Focus, back: bool) -> Focus {
    match (f, back) {
        (Focus::Table, false) | (Focus::Activity, true) => Focus::Log,
        (Focus::Log, false) | (Focus::Table, true) => Focus::Activity,
        (Focus::Activity, false) | (Focus::Log, true) => Focus::Table,
    }
}

/// Rows inside a bordered pane; the top and bottom border take two.
fn viewport_height(area_h: u16) -> usize {
    area_h.saturating_sub(2) as usize
}

/// How far a view can scroll back from the tail before its top is reached.
fn max_offset(total: usize, viewport: usize) -> usize {
    total.saturating_sub(viewport)
}

fn clamp_offset(off: usize, total: usize, viewport: usize) -> usize {
    off.min(max_offset(total, viewport))
}

/// The lines a view shows when it is `from_bottom` lines above the tail.
/// The offset is clamped first, so content shrinking under it cannot make
/// the range underflow.
fn window(total: usize, viewport: usize, from_bottom: usize) -> Range<usize> {
    let off = clamp_offset(from_bottom, total, viewport);
    let end = total - off;
    let start = end.saturating_sub(viewport);
    start..end
}

/// New scroll offset for a log view counted from the tail (0 = following).
/// Paging moves by one screen less a line, so the last line stays in view.
fn scroll_key(code: KeyCode, off: usize, total: usize, viewport: usize) -> usize {
    let page = viewport.saturating_sub(1).max(1);
    let next = match code {
        KeyCode::Up | KeyCode::Char('k') => off.saturating_add(1),
        KeyCode::Down | KeyCode::Char('j') => off.saturating_sub(1),
        KeyCode::PageUp => off.saturating_add(page),
        KeyCode::PageDown => off.saturating_sub(page),
        KeyCode::Home => max_offset(total, viewport),
        KeyCode::End => 0,
        _ => off,
    };
    clamp_offset(next, total, viewport)
}

/// Keeps a tail-relative view still while lines arrive. A view at the tail
/// keeps following; a scrolled one moves up by the number of new lines so it
/// shows the same content. Eviction from the front is handled by the caller's
/// clamp.
fn follow_adjust(off: usize, new_seq: u64, old_seq: u64) -> usize {
    if off == 0 {
        return 0;
    }
    let arrived = usize::try_from(new_seq.saturating_sub(old_seq)).unwrap_or(usize::MAX);
    off.saturating_add(arrived)
}

/// Case-insensitive substring match. An empty needle matches everything.
#[cfg_attr(not(test), allow(dead_code))]
fn filter_matches(line: &str, needle: &str) -> bool {
    needle.is_empty() || line.to_lowercase().contains(&needle.to_lowercase())
}

/// Indices of the log lines that pass `filter_matches`, in order.
#[cfg_attr(not(test), allow(dead_code))]
fn filtered_indices(lines: &[LogLine], needle: &str) -> Vec<usize> {
    (0..lines.len())
        .filter(|&i| filter_matches(&lines[i].message, needle))
        .collect()
}

/// New selection in the Agent Activity list (index into the entries, 0 = oldest).
fn activity_move(sel: usize, total: usize, code: KeyCode, viewport: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let last = total - 1;
    let sel = sel.min(last);
    let page = viewport.saturating_sub(1).max(1);
    match code {
        KeyCode::Up | KeyCode::Char('k') => sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => (sel + 1).min(last),
        KeyCode::PageUp => sel.saturating_sub(page),
        KeyCode::PageDown => (sel + page).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => sel,
    }
}

/// First visible entry after keeping `sel` on screen with the smallest scroll
/// that does it.
fn activity_top(sel: usize, top: usize, total: usize, viewport: usize) -> usize {
    if total == 0 || viewport == 0 {
        return 0;
    }
    let sel = sel.min(total - 1);
    let top = clamp_offset(top, total, viewport);
    let top = if sel < top {
        sel
    } else if sel >= top + viewport {
        sel + 1 - viewport
    } else {
        top
    };
    clamp_offset(top, total, viewport)
}

/// Like `scroll_key`, but for text read from the top: `off` is the first
/// visible line, so Down moves towards the end.
fn text_scroll_key(code: KeyCode, off: usize, total: usize, viewport: usize) -> usize {
    let page = viewport.saturating_sub(1).max(1);
    let next = match code {
        KeyCode::Up | KeyCode::Char('k') => off.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => off.saturating_add(1),
        KeyCode::PageUp => off.saturating_sub(page),
        KeyCode::PageDown => off.saturating_add(page),
        KeyCode::Home => 0,
        KeyCode::End => max_offset(total, viewport),
        _ => off,
    };
    clamp_offset(next, total, viewport)
}

#[derive(Debug, PartialEq, Eq)]
enum EscAction {
    /// Cancel the `/` prompt, keeping any applied filter.
    ClosePrompt,
    ClearFilter,
    /// Close the expanded entry.
    Collapse,
    Quit,
}

/// What Esc does. Transient UI is dismissed before anything quits, in this
/// order, so Esc only ends the run once nothing else is open.
fn esc_action(browse: &BrowseState) -> EscAction {
    if browse.filter_input.is_some() {
        EscAction::ClosePrompt
    } else if browse.filter.is_some() {
        EscAction::ClearFilter
    } else if browse.act_expanded {
        EscAction::Collapse
    } else {
        EscAction::Quit
    }
}

fn render(
    f: &mut Frame,
    state: &TuiState,
    table_state: &mut TableState,
    input: Option<&str>,
    browse: &BrowseState,
) -> RenderFeedback {
    let visible = state.visible_order();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                                       // Summary header
            Constraint::Length((visible.len() as u16 + 3).clamp(5, 12)), // Package table
            Constraint::Min(8),                                          // Detail pane
            Constraint::Length(3),                                       // Global log / footer
        ])
        .split(f.area());

    render_summary(f, state, chunks[0]);
    render_table(
        f,
        state,
        &visible,
        chunks[1],
        table_state,
        browse.focus == Focus::Table,
    );
    let feedback = render_detail(
        f,
        state,
        &visible,
        chunks[2],
        table_state.selected(),
        browse,
    );
    let retryable = table_state
        .selected()
        .and_then(|i| visible.get(i))
        .and_then(|name| state.packages.get(name.as_str()))
        .is_some_and(|p| p.status == PkgStatus::NoSpace && !p.retry_requested);
    render_footer(f, state, input, browse, chunks[3], retryable);
    feedback
}

/// A bordered pane. The focused one gets a yellow border and a `▶` in its
/// title, so focus still shows without colour.
fn pane(title: &str, focused: bool) -> Block<'static> {
    let block = Block::default().borders(Borders::ALL);
    if focused {
        block
            .border_style(Style::default().fg(Color::Yellow))
            .title(format!("▶ {title}"))
    } else {
        block.title(title.to_string())
    }
}

fn render_summary(f: &mut Frame, state: &TuiState, area: Rect) {
    let done = state.packages.values().filter(|p| p.finished).count();
    let total = state.order.len();
    // Live, not the `ResolveFinished` snapshot: that event only fires once
    // per batch re-resolution of a root's own pip closure, which the far
    // more frequent ad hoc dependency builds never trigger — trusting it
    // here left this reading "0 built" for most of a long run even as
    // packages visibly succeeded in the table below. `in_distro` has no
    // live equivalent (a distro-satisfied dependency never becomes a row
    // here at all), so that one is still the last snapshot.
    let built = state
        .packages
        .values()
        .filter(|p| matches!(p.status, PkgStatus::Succeeded | PkgStatus::AlreadyBuilt))
        .count();
    let elapsed = state.run_start.map(|s| s.elapsed().as_secs()).unwrap_or(0);
    let title = match (state.resolve_total, state.resolve_in_distro) {
        (Some(closure_total), Some(in_distro)) => format!(
            " Auto-Package — {done}/{total} packages done ({built} built, {in_distro}/{closure_total} in distro) │ elapsed {elapsed}s │ cost ${:.4} ",
            state.total_cost_usd()
        ),
        _ => format!(
            " Auto-Package — {done}/{total} packages done ({built} built) │ elapsed {elapsed}s │ cost ${:.4} ",
            state.total_cost_usd()
        ),
    };
    let header = Paragraph::new(title)
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, area);
}

fn render_table(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    table_state: &mut TableState,
    focused: bool,
) {
    let rows: Vec<Row> = visible
        .iter()
        .map(|name| {
            let pkg = &state.packages[*name];
            let step = pkg.step_text();
            let elapsed = pkg.started.elapsed().as_secs();
            let tries = if pkg.max_tries > 0 {
                format!("{}/{}", pkg.tries, pkg.max_tries)
            } else {
                "-".to_string()
            };
            Row::new(vec![
                Cell::from(pkg.status.label()).style(Style::default().fg(pkg.status.color())),
                Cell::from(name.as_str()),
                Cell::from(pkg.version.as_str()),
                Cell::from(step),
                Cell::from(tries),
                Cell::from(format!("{elapsed}s")),
                Cell::from(format!("${:.4}", pkg.cost_usd)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(11),
        Constraint::Percentage(30),
        Constraint::Length(12),
        Constraint::Percentage(30),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(9),
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec![
                "Status", "Package", "Version", "Step", "Tries", "Time", "Cost",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(pane("Packages", focused));

    f.render_stateful_widget(table, area, table_state);
}

fn render_detail(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    selected: Option<usize>,
    browse: &BrowseState,
) -> RenderFeedback {
    if browse.show_lists {
        return render_lists(f, state, area, browse);
    }
    let pkg = selected
        .and_then(|i| visible.get(i))
        .and_then(|n| state.packages.get(n.as_str()));

    // A failure reason gets its own strip above the logs; without one the
    // panes take the whole area, as before.
    let body = match pkg.and_then(|p| p.fail_reason.as_ref().map(|r| (p, r))) {
        Some((pkg, reason)) => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(4), Constraint::Min(0)])
                .split(area);
            let title = match pkg.step.as_ref() {
                Some(step) if step.round > 1 => {
                    format!("Failed at {} (round {})", step.kind.label(), step.round)
                }
                Some(step) => format!("Failed at {}", step.kind.label()),
                None => "Failed".to_string(),
            };
            f.render_widget(
                Paragraph::new(reason.as_str())
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: true })
                    .block(Block::default().borders(Borders::ALL).title(title)),
                rows[0],
            );
            rows[1]
        }
        None => area,
    };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(body);

    let Some(pkg) = pkg else {
        f.render_widget(
            Paragraph::new("No package selected yet")
                .block(pane("Event Log", browse.focus == Focus::Log)),
            chunks[0],
        );
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Agent Activity"),
            chunks[1],
        );
        return RenderFeedback::default();
    };

    let log_viewport = viewport_height(chunks[0].height);
    // The filter narrows the log before windowing, so the offset and the
    // "more below" count are in filtered lines.
    let shown: Vec<usize> = match browse.filter.as_deref() {
        Some(needle) => filtered_indices(&pkg.log_lines, needle),
        None => (0..pkg.log_lines.len()).collect(),
    };
    let total = shown.len();
    let scroll = clamp_offset(browse.log_off, total, log_viewport);
    let log_items: Vec<ListItem> = shown[window(total, log_viewport, scroll)]
        .iter()
        .map(|&i| &pkg.log_lines[i])
        .map(|l| {
            let mut style = Style::default().fg(l.level.color());
            if l.emphasis {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(l.message.clone()).style(style)
        })
        .collect();
    let filter_note = browse
        .filter
        .as_deref()
        .map(|needle| format!(" [filter: {needle}]"))
        .unwrap_or_default();
    let mut log_title = if scroll > 0 {
        format!("Event Log{filter_note} — {scroll} more below, End to follow")
    } else {
        format!("Event Log{filter_note}")
    };
    if !pkg.skipped.is_empty() {
        let labels: Vec<&str> = pkg.skipped.iter().map(|(k, _)| k.label()).collect();
        log_title.push_str(&format!(" (skipped: {})", labels.join(", ")));
    }
    let log = List::new(log_items).block(pane(&log_title, browse.focus == Focus::Log));

    let act_viewport = viewport_height(chunks[1].height);
    let act_len = pkg.agent_activity.len();
    let act_sel = browse.act_sel.min(act_len.saturating_sub(1));
    let act_top = activity_top(act_sel, browse.act_top, act_len, act_viewport);
    let mut feedback = RenderFeedback {
        log_total: total,
        log_viewport,
        log_off: scroll,
        act_len,
        act_viewport,
        act_top,
        ..RenderFeedback::default()
    };

    // An expanded entry takes the Event Log's place, so its full text can be
    // read in the wider pane.
    let expanded = if browse.act_expanded {
        pkg.agent_activity.get(act_sel)
    } else {
        None
    };
    if let Some(entry) = expanded {
        let text_viewport = viewport_height(chunks[0].height);
        let text = wrap_text(&entry.text, chunks[0].width.saturating_sub(2) as usize);
        let off = clamp_offset(browse.detail_off, text.len(), text_viewport);
        let end = (off + text_viewport).min(text.len());
        let lines: Vec<Line> = text[off..end]
            .iter()
            .map(|l| Line::styled(l.as_str(), Style::default().fg(entry.kind.color())))
            .collect();
        let title = format!("{} — Esc close, ↑/↓ scroll", entry.kind.label());
        f.render_widget(
            Paragraph::new(lines).block(pane(&title, browse.focus == Focus::Activity)),
            chunks[0],
        );
        feedback.detail_total = text.len();
        feedback.detail_viewport = text_viewport;
        feedback.detail_off = off;
    } else {
        f.render_widget(log, chunks[0]);
    }

    let inner_w = chunks[1].width.saturating_sub(2) as usize;
    let activity_items: Vec<ListItem> = pkg.agent_activity
        [act_top..(act_top + act_viewport).min(act_len)]
        .iter()
        .map(|a| {
            // Entries can span lines; the list shows the first one, and the
            // full text is one Enter away.
            let first = a.text.lines().next().unwrap_or("");
            ListItem::new(truncate_with_ellipsis(first, inner_w))
                .style(Style::default().fg(a.kind.color()))
        })
        .collect();
    let mut list_state = ListState::default();
    if browse.focus == Focus::Activity && act_len > 0 {
        list_state.select(Some(act_sel - act_top));
    }
    let activity_title = if act_len > 0 {
        format!(
            "Agent Activity {}/{act_len} — {} in / {} out",
            act_sel + 1,
            pkg.input_tokens,
            pkg.output_tokens
        )
    } else {
        format!(
            "Agent Activity — {} in / {} out",
            pkg.input_tokens, pkg.output_tokens
        )
    };
    let activity = List::new(activity_items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(pane(&activity_title, browse.focus == Focus::Activity));
    f.render_stateful_widget(activity, chunks[1], &mut list_state);
    feedback
}

/// `name <- dep1, dep2` for each package waiting on dependencies, in table
/// order; just `name` when the outcome named none.
fn queued_lines(state: &TuiState) -> Vec<String> {
    state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .filter(|(_, p)| p.status == PkgStatus::MissingDeps)
        .map(|(n, p)| {
            if p.waiting_on.is_empty() {
                n.clone()
            } else {
                format!("{n} <- {}", p.waiting_on.join(", "))
            }
        })
        .collect()
}

/// `name version` for each package that is built, in table order.
fn built_lines(state: &TuiState) -> Vec<String> {
    state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .filter(|(_, p)| matches!(p.status, PkgStatus::Succeeded | PkgStatus::AlreadyBuilt))
        .map(|(n, p)| format!("{n} {}", p.version).trim_end().to_string())
        .collect()
}

/// The Queued and Built lists side by side in place of the detail panes. One
/// offset scrolls both, clamped to the longer, so they stay aligned.
fn render_lists(
    f: &mut Frame,
    state: &TuiState,
    area: Rect,
    browse: &BrowseState,
) -> RenderFeedback {
    let queued = queued_lines(state);
    let built = built_lines(state);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let viewport = viewport_height(area.height);
    let total = queued.len().max(built.len());
    let off = clamp_offset(browse.lists_off, total, viewport);
    for (lines, title, col) in [
        (&queued, format!("Queued ({})", queued.len()), cols[0]),
        (&built, format!("Built ({})", built.len()), cols[1]),
    ] {
        let end = (off + viewport).min(lines.len());
        let items: Vec<ListItem> = lines[off.min(end)..end]
            .iter()
            .map(|l| ListItem::new(l.as_str()))
            .collect();
        f.render_widget(
            List::new(items).block(pane(&title, browse.focus == Focus::Log)),
            col,
        );
    }
    RenderFeedback {
        lists_total: total,
        lists_viewport: viewport,
        ..RenderFeedback::default()
    }
}

/// One line of a text prompt: label, the text typed so far with a cursor, and
/// the keys that finish or cancel it.
fn prompt_line(label: &str, buf: &str, keys: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(Color::Cyan)),
        Span::raw(buf.to_string()),
        Span::styled("█", Style::default().fg(Color::Cyan)),
        Span::styled(keys.to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

fn render_footer(
    f: &mut Frame,
    state: &TuiState,
    input: Option<&str>,
    browse: &BrowseState,
    area: Rect,
    retryable: bool,
) {
    let prompt = match (input, browse.filter_input.as_deref()) {
        (Some(buf), _) => Some(prompt_line(
            "add package (name or repo URL): ",
            buf,
            "   Enter queue │ Esc cancel",
        )),
        (None, Some(buf)) => Some(prompt_line(
            "filter log: ",
            buf,
            "   Enter apply │ Esc cancel",
        )),
        (None, None) => None,
    };
    if let Some(line) = prompt {
        f.render_widget(
            Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    let last = state
        .last_global_log
        .as_ref()
        .map(|l| Span::styled(l.message.clone(), Style::default().fg(l.level.color())))
        .unwrap_or_else(|| Span::raw(""));
    let nav = match browse.focus {
        Focus::Table => "↑/↓ select │ PgUp/PgDn/Home/End scroll log",
        Focus::Log => "↑/↓ PgUp/PgDn/Home/End scroll log │ / filter",
        Focus::Activity if browse.act_expanded => "↑/↓ PgUp/PgDn/Home/End scroll text",
        Focus::Activity => "↑/↓ PgUp/PgDn/Home/End move │ Enter expand",
    };
    // Esc does the first thing still open, so say what it does now.
    let quit = match esc_action(browse) {
        EscAction::ClosePrompt => "Esc cancel",
        EscAction::ClearFilter => "Esc clear filter │ q quit",
        EscAction::Collapse => "Esc close │ q quit",
        EscAction::Quit => "q/Esc quit",
    };
    let hint = Span::styled(
        format!(
            "  {nav} │ Tab focus │ l lists │ n add package{} │ a {} succeeded │ {quit}",
            if retryable {
                " │ r retry (no space left)"
            } else {
                ""
            },
            if state.show_all { "hide" } else { "show" }
        ),
        Style::default().fg(Color::DarkGray),
    );
    let line = Line::from(vec![last, hint]);
    let footer = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    f.render_widget(footer, area);
}

#[cfg(test)]
mod disk_full_tests {
    use super::*;

    #[test]
    fn matches_the_literal_strerror_text() {
        assert!(looks_like_disk_full(
            "error: Bad exit status: cpio: write: No space left on device"
        ));
    }

    #[test]
    fn matches_case_insensitively() {
        assert!(looks_like_disk_full("NO SPACE LEFT ON DEVICE"));
    }

    #[test]
    fn an_unrelated_failure_does_not_match() {
        assert!(!looks_like_disk_full(
            "CMake Error: The LOCATION property may not be read from target \"png\""
        ));
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::*;

    #[test]
    fn short_text_is_returned_unchanged() {
        assert_eq!(truncate_with_ellipsis("cargo build", 80), "cargo build");
    }

    #[test]
    fn wide_characters_count_as_two_columns() {
        // Nine columns: six for the content, three for "...".
        let s = "日本語テキスト";
        let out = truncate_with_ellipsis(s, 9);
        assert_eq!(out, "日本語...");
        assert!(out.width() <= 9);
    }

    #[test]
    fn a_wide_character_that_does_not_fit_is_dropped_whole() {
        assert_eq!(prefix_within_width("ab日本", 3), "ab");
    }

    #[test]
    fn zero_width_joiner_sequences_are_not_split() {
        // Woman + ZWJ + man + ZWJ + girl: one cluster, six columns wide.
        let family = "👩\u{200d}👨\u{200d}👧";
        assert_eq!(prefix_within_width(family, 6), family);
        assert_eq!(prefix_within_width(family, 5), "");
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        let s = "e\u{301}x";
        assert_eq!(prefix_within_width(s, 1), "e\u{301}");
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    fn type_str(buf: &mut String, s: &str) {
        for c in s.chars() {
            assert!(matches!(
                prompt_key(buf, KeyCode::Char(c)),
                PromptAction::Pending
            ));
        }
    }

    #[test]
    fn typing_q_or_j_edits_the_buffer_not_the_ui() {
        let mut b = String::new();
        type_str(&mut b, "jq-cpp");
        assert_eq!(b, "jq-cpp");
    }

    #[test]
    fn enter_submits_the_trimmed_name() {
        let mut b = String::new();
        type_str(&mut b, "  oatpp ");
        assert!(matches!(
            prompt_key(&mut b, KeyCode::Enter),
            PromptAction::Submit(n) if n == "oatpp"
        ));
    }

    #[test]
    fn enter_on_blank_and_esc_cancel_and_backspace_edits() {
        let mut b = String::from("  ");
        assert!(matches!(
            prompt_key(&mut b, KeyCode::Enter),
            PromptAction::Cancel
        ));
        let mut b = String::from("ab");
        assert!(matches!(
            prompt_key(&mut b, KeyCode::Backspace),
            PromptAction::Pending
        ));
        assert_eq!(b, "a");
        assert!(matches!(
            prompt_key(&mut b, KeyCode::Esc),
            PromptAction::Cancel
        ));
    }
}

#[cfg(test)]
mod step_state_tests {
    use super::*;
    use ap_core::event::{SelectionReason, StepId};

    fn msg(pkg: &str, event: Event) -> Msg {
        Msg {
            pkg: Some(pkg.to_string()),
            event,
        }
    }

    fn started(kind: StepKind) -> Event {
        Event::StepStarted {
            step: StepId(0),
            kind,
            attempt: 1,
            round: 0,
            detail: None,
        }
    }

    fn finished(outcome: StepOutcome, secs: u64) -> Event {
        Event::StepFinished {
            step: StepId(0),
            outcome,
            elapsed: Duration::from_secs(secs),
        }
    }

    fn package_finished(outcome: PackageOutcome) -> Event {
        Event::PackageFinished {
            pkg: "foo".to_string(),
            version: "1.0".to_string(),
            outcome,
        }
    }

    #[test]
    fn missing_deps_sets_waiting_on_and_a_later_build_clears_it() {
        let mut state = TuiState::new();
        let deps = ["b", "a", "b"].map(String::from).to_vec();
        state.apply(msg(
            "foo",
            package_finished(PackageOutcome::MissingDeps { deps }),
        ));
        assert_eq!(state.packages["foo"].waiting_on, ["a", "b"]);
        state.apply(msg("foo", package_finished(PackageOutcome::Built)));
        assert!(state.packages["foo"].waiting_on.is_empty());
    }

    #[test]
    fn queued_and_built_lines_follow_table_order() {
        let mut state = TuiState::new();
        let fin = |pkg: &str, outcome| Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "2.0".to_string(),
            outcome,
        };
        let missing = |deps: &[&str]| PackageOutcome::MissingDeps {
            deps: deps.iter().map(|d| d.to_string()).collect(),
        };
        state.apply(msg("w", fin("w", missing(&["y", "x"]))));
        state.apply(msg("v", fin("v", PackageOutcome::Built)));
        state.apply(msg("u", fin("u", missing(&[]))));
        state.apply(msg("t", fin("t", PackageOutcome::AlreadyBuilt)));
        assert_eq!(queued_lines(&state), ["w <- x, y", "u"]);
        assert_eq!(built_lines(&state), ["v 2.0", "t 2.0"]);
    }

    #[test]
    fn a_finished_step_between_steps_shows_its_label_not_a_dash() {
        let mut state = TuiState::new();
        state.apply(msg("foo", started(StepKind::Checkout)));
        state.apply(msg("foo", finished(StepOutcome::Ok, 2)));
        let pkg = &state.packages["foo"];
        assert!(pkg.status == PkgStatus::Between);
        let text = pkg.step_text();
        assert!(text.contains(StepKind::Checkout.label()));
        assert_ne!(text, "-");
    }

    #[test]
    fn a_failed_step_then_failed_package_keeps_the_step_and_reason() {
        let mut state = TuiState::new();
        state.apply(msg("foo", started(StepKind::Checkout)));
        state.apply(msg("foo", finished(StepOutcome::Failed, 2)));
        state.apply(Msg {
            pkg: None,
            event: package_finished(PackageOutcome::Failed {
                reason: "boom".to_string(),
            }),
        });
        let pkg = &state.packages["foo"];
        assert!(pkg.status == PkgStatus::Failed);
        let text = pkg.step_text();
        assert!(text.starts_with('✗'));
        assert!(text.contains(StepKind::Checkout.label()));
        assert_eq!(pkg.fail_reason.as_deref(), Some("boom"));
    }

    #[test]
    fn a_built_package_has_no_step() {
        let mut state = TuiState::new();
        state.apply(msg("foo", started(StepKind::Checkout)));
        state.apply(msg("foo", finished(StepOutcome::Ok, 2)));
        state.apply(Msg {
            pkg: None,
            event: package_finished(PackageOutcome::Built),
        });
        let pkg = &state.packages["foo"];
        assert!(pkg.step.is_none());
        assert_eq!(pkg.step_text(), "-");
    }

    #[test]
    fn a_new_attempt_clears_the_previous_failure() {
        let mut state = TuiState::new();
        state.apply(msg("foo", started(StepKind::Checkout)));
        state.apply(msg("foo", finished(StepOutcome::Failed, 2)));
        state.apply(Msg {
            pkg: None,
            event: package_finished(PackageOutcome::Failed {
                reason: "boom".to_string(),
            }),
        });
        state.apply(Msg {
            pkg: None,
            event: Event::TargetSelected {
                pkg: "foo".to_string(),
                version: "1.0".to_string(),
                reason: SelectionReason::RequestedRoot,
                blocked_by: Vec::new(),
            },
        });
        let pkg = &state.packages["foo"];
        assert!(pkg.step.is_none());
        assert!(pkg.fail_reason.is_none());
    }

    fn target_selected() -> Msg {
        Msg {
            pkg: None,
            event: Event::TargetSelected {
                pkg: "foo".to_string(),
                version: "1.0".to_string(),
                reason: SelectionReason::RequestedRoot,
                blocked_by: Vec::new(),
            },
        }
    }

    #[test]
    fn a_retry_resets_finished_and_tries() {
        let mut state = TuiState::new();
        state.apply(msg(
            "foo",
            Event::AgentPrompt {
                step: StepId(0),
                task: Task::FixAfterBuild {
                    attempt: 2,
                    max_attempts: 3,
                },
                prompt: String::new(),
                artifact: std::path::PathBuf::new(),
            },
        ));
        state.apply(Msg {
            pkg: None,
            event: package_finished(PackageOutcome::Failed {
                reason: "boom".to_string(),
            }),
        });
        assert!(state.packages["foo"].finished);
        state.apply(target_selected());
        let pkg = &state.packages["foo"];
        assert!(!pkg.finished);
        assert_eq!((pkg.tries, pkg.max_tries), (0, 0));
    }

    #[test]
    fn disk_full_only_classifies_the_failure_that_hit_it() {
        let mut state = TuiState::new();
        state
            .packages
            .insert("foo".into(), PkgState::new("1.0".into()));
        state.packages.get_mut("foo").unwrap().disk_full_detected = true;
        state.apply(msg("foo", started(StepKind::Build)));
        state.apply(Msg {
            pkg: None,
            event: package_finished(PackageOutcome::Failed {
                reason: "boom".to_string(),
            }),
        });
        assert!(state.packages["foo"].status == PkgStatus::Failed);
    }

    #[test]
    fn a_repeated_step_shows_its_attempt_number() {
        let mut state = TuiState::new();
        state.apply(msg(
            "foo",
            Event::StepStarted {
                step: StepId(1),
                kind: StepKind::Build,
                attempt: 2,
                round: 2,
                detail: None,
            },
        ));
        let text = state.packages["foo"].step_text();
        assert!(text.contains("#2"), "got {text:?}");
    }

    #[test]
    fn a_skipped_step_is_recorded_without_replacing_the_current_step() {
        let mut state = TuiState::new();
        state.apply(msg("foo", started(StepKind::Checkout)));
        state.apply(msg(
            "foo",
            Event::StepSkipped {
                kind: StepKind::OscService,
                reason: SkipReason::NotRust,
            },
        ));
        let pkg = &state.packages["foo"];
        assert_eq!(pkg.skipped, [(StepKind::OscService, SkipReason::NotRust)]);
        assert_eq!(pkg.step.as_ref().unwrap().kind, StepKind::Checkout);
    }
}

#[cfg(test)]
mod browse_tests {
    use super::*;

    fn line(message: &str) -> LogLine {
        LogLine {
            level: LogLevel::Info,
            message: message.to_string(),
            emphasis: false,
        }
    }

    #[test]
    fn window_of_an_empty_log_is_empty() {
        assert_eq!(window(0, 5, 0), 0..0);
        assert_eq!(window(0, 5, 3), 0..0);
    }

    #[test]
    fn window_shorter_than_the_viewport_shows_everything() {
        assert_eq!(window(3, 5, 0), 0..3);
        assert_eq!(window(3, 5, 2), 0..3);
    }

    #[test]
    fn window_equal_to_the_viewport_shows_everything() {
        assert_eq!(window(5, 5, 0), 0..5);
        assert_eq!(window(5, 5, 1), 0..5);
    }

    #[test]
    fn window_offset_past_the_top_is_clamped() {
        assert_eq!(window(10, 4, 100), 0..4);
        assert_eq!(window(10, 4, 6), 0..4);
        assert_eq!(window(10, 4, 0), 6..10);
        assert_eq!(window(10, 4, 2), 4..8);
    }

    #[test]
    fn window_with_zero_viewport_is_empty_and_never_underflows() {
        assert_eq!(window(5, 0, 2), 3..3);
    }

    #[test]
    fn scroll_up_at_the_top_stays_and_down_at_the_tail_stays() {
        assert_eq!(scroll_key(KeyCode::Up, 6, 10, 4), 6);
        assert_eq!(scroll_key(KeyCode::Down, 0, 10, 4), 0);
        assert_eq!(scroll_key(KeyCode::Char('k'), 0, 10, 4), 1);
        assert_eq!(scroll_key(KeyCode::Char('j'), 3, 10, 4), 2);
    }

    #[test]
    fn paging_moves_by_viewport_minus_one_with_a_floor_of_one() {
        assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 5), 4);
        assert_eq!(scroll_key(KeyCode::PageDown, 4, 10, 5), 0);
        assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 1), 1);
        assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 0), 1);
        assert_eq!(scroll_key(KeyCode::PageDown, 0, 10, 0), 0);
    }

    #[test]
    fn home_is_the_oldest_line_and_end_is_the_tail() {
        assert_eq!(scroll_key(KeyCode::Home, 0, 10, 4), 6);
        assert_eq!(scroll_key(KeyCode::End, 6, 10, 4), 0);
        assert_eq!(scroll_key(KeyCode::Home, 0, 0, 4), 0);
    }

    #[test]
    fn scroll_offset_is_clamped_when_content_shrinks() {
        assert_eq!(scroll_key(KeyCode::PageUp, 50, 10, 4), 6);
        assert_eq!(scroll_key(KeyCode::Up, 50, 10, 4), 6);
    }

    #[test]
    fn a_view_at_the_tail_keeps_following() {
        assert_eq!(follow_adjust(0, 10, 4), 0);
    }

    #[test]
    fn a_scrolled_view_moves_up_by_the_lines_that_arrived() {
        assert_eq!(follow_adjust(5, 3, 0), 8);
        assert_eq!(follow_adjust(5, 7, 7), 5);
    }

    #[test]
    fn eviction_after_arrivals_is_left_to_the_clamp() {
        let adjusted = follow_adjust(8, 3, 0);
        assert_eq!(adjusted, 11);
        assert_eq!(clamp_offset(adjusted, 10, 4), 6);
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        assert!(filter_matches("Building jq-cpp", "JQ"));
        assert!(filter_matches("über cool", "ÜBER"));
        assert!(!filter_matches("nothing here", "xyz"));
    }

    #[test]
    fn empty_filter_matches_every_line() {
        assert!(filter_matches("", ""));
        assert!(filter_matches("anything", ""));
    }

    #[test]
    fn filtered_indices_keep_log_order() {
        let lines = [line("a"), line("B a"), line("c")];
        assert_eq!(filtered_indices(&lines, "a"), vec![0, 1]);
        assert_eq!(filtered_indices(&lines, ""), vec![0, 1, 2]);
        assert!(filtered_indices(&lines, "zzz").is_empty());
    }

    #[test]
    fn focus_cycles_both_ways_and_wraps() {
        assert_eq!(cycle_focus(Focus::Table, false), Focus::Log);
        assert_eq!(cycle_focus(Focus::Log, false), Focus::Activity);
        assert_eq!(cycle_focus(Focus::Activity, false), Focus::Table);
        assert_eq!(cycle_focus(Focus::Table, true), Focus::Activity);
        assert_eq!(cycle_focus(Focus::Activity, true), Focus::Log);
        assert_eq!(cycle_focus(Focus::Log, true), Focus::Table);
    }

    #[test]
    fn activity_selection_stays_inside_an_empty_or_short_list() {
        assert_eq!(activity_move(0, 0, KeyCode::Down, 5), 0);
        assert_eq!(activity_move(4, 2, KeyCode::Down, 5), 1);
        assert_eq!(activity_move(9, 3, KeyCode::Up, 5), 1);
    }

    #[test]
    fn activity_selection_moves_and_jumps() {
        assert_eq!(activity_move(1, 10, KeyCode::Down, 5), 2);
        assert_eq!(activity_move(0, 10, KeyCode::Up, 5), 0);
        assert_eq!(activity_move(0, 10, KeyCode::PageDown, 3), 2);
        assert_eq!(activity_move(5, 10, KeyCode::Home, 3), 0);
        assert_eq!(activity_move(5, 10, KeyCode::End, 3), 9);
        assert_eq!(activity_move(9, 10, KeyCode::Down, 3), 9);
    }

    #[test]
    fn activity_top_is_zero_for_an_empty_list() {
        assert_eq!(activity_top(0, 3, 0, 5), 0);
    }

    #[test]
    fn activity_top_scrolls_the_minimum_to_show_the_selection() {
        assert_eq!(activity_top(7, 0, 17, 5), 3);
        assert_eq!(activity_top(1, 4, 17, 5), 1);
        assert_eq!(activity_top(5, 3, 17, 5), 3);
        assert_eq!(activity_top(16, 0, 17, 5), 12);
    }

    #[test]
    fn activity_selection_stays_visible_after_the_viewport_shrinks() {
        let top = activity_top(7, 5, 17, 2);
        assert!((top..top + 2).contains(&7));
    }
}

#[cfg(test)]
mod seq_tests {
    use super::*;

    #[test]
    fn log_seq_keeps_counting_after_eviction_caps_the_log() {
        let mut pkg = PkgState::new(String::new());
        for i in 0..MAX_LOG_LINES + 5 {
            pkg.add_log(LogLevel::Info, i.to_string());
        }
        assert_eq!(pkg.log_lines.len(), MAX_LOG_LINES);
        assert_eq!(pkg.log_seq, (MAX_LOG_LINES + 5) as u64);
    }

    #[test]
    fn activity_seq_keeps_counting_after_eviction_caps_the_list() {
        let mut pkg = PkgState::new(String::new());
        for i in 0..MAX_AGENT_ACTIVITY + 3 {
            pkg.add_agent_activity(ActivityKind::Task, i.to_string());
        }
        assert_eq!(pkg.agent_activity.len(), MAX_AGENT_ACTIVITY);
        assert_eq!(pkg.activity_seq, (MAX_AGENT_ACTIVITY + 3) as u64);
    }
}

#[cfg(test)]
mod expand_tests {
    use super::*;

    #[test]
    fn wrap_breaks_between_words() {
        assert_eq!(wrap_text("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_text("one two three", 5), ["one", "two", "three"]);
    }

    #[test]
    fn wrap_breaks_a_word_longer_than_the_line() {
        assert_eq!(wrap_text("abcdefgh", 3), ["abc", "def", "gh"]);
    }

    #[test]
    fn wrap_keeps_newlines_and_blank_lines() {
        assert_eq!(wrap_text("a\n\nb", 10), ["a", "", "b"]);
    }

    #[test]
    fn wrap_of_empty_text_has_no_lines() {
        assert!(wrap_text("", 10).is_empty());
    }

    #[test]
    fn wrap_counts_wide_characters_as_two_columns() {
        assert_eq!(wrap_text("日本語", 4), ["日本", "語"]);
    }

    #[test]
    fn wrap_gives_a_glyph_wider_than_the_line_its_own_line() {
        assert_eq!(wrap_text("日", 1), ["日"]);
    }

    #[test]
    fn text_scroll_stays_inside_the_text() {
        assert_eq!(text_scroll_key(KeyCode::Up, 0, 10, 4), 0);
        assert_eq!(text_scroll_key(KeyCode::Down, 0, 10, 4), 1);
        assert_eq!(text_scroll_key(KeyCode::Down, 6, 10, 4), 6);
        assert_eq!(text_scroll_key(KeyCode::PageDown, 0, 10, 4), 3);
        assert_eq!(text_scroll_key(KeyCode::End, 0, 10, 4), 6);
        assert_eq!(text_scroll_key(KeyCode::Home, 6, 10, 4), 0);
    }

    #[test]
    fn esc_closes_an_open_entry_before_quitting() {
        let mut browse = BrowseState::default();
        assert_eq!(esc_action(&browse), EscAction::Quit);
        browse.act_expanded = true;
        assert_eq!(esc_action(&browse), EscAction::Collapse);
    }
}

#[cfg(test)]
mod filter_esc_tests {
    use super::*;

    #[test]
    fn esc_dismisses_the_prompt_then_the_filter_then_the_entry_then_quits() {
        let mut browse = BrowseState::default();
        browse.act_expanded = true;
        browse.filter = Some("jq".to_string());
        browse.filter_input = Some(String::new());
        assert_eq!(esc_action(&browse), EscAction::ClosePrompt);
        browse.filter_input = None;
        assert_eq!(esc_action(&browse), EscAction::ClearFilter);
        browse.filter = None;
        assert_eq!(esc_action(&browse), EscAction::Collapse);
        browse.act_expanded = false;
        assert_eq!(esc_action(&browse), EscAction::Quit);
    }

    #[test]
    fn selecting_another_package_drops_its_filter() {
        let mut browse = BrowseState::default();
        browse.filter = Some("jq".to_string());
        browse.filter_input = Some("j".to_string());
        browse.reset_for_package(0);
        assert!(browse.filter.is_none());
        assert!(browse.filter_input.is_none());
    }
}

#[cfg(test)]
mod render_smoke_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn state_with_history() -> TuiState {
        let mut state = TuiState::new();
        let pkg = state.pkg_mut("foo");
        for i in 0..50 {
            pkg.add_log(LogLevel::Info, format!("log line {i} with some text"));
        }
        for i in 0..30 {
            pkg.add_agent_activity(ActivityKind::ToolUse, format!("→ Read {i}\nsecond line"));
        }
        pkg.add_agent_activity(
            ActivityKind::Reply,
            format!("done (confidence: high)\n{}", "long reasoning ".repeat(40)),
        );
        state
    }

    #[test]
    fn every_focus_and_size_renders_without_panicking() {
        let state = state_with_history();
        for (w, h) in [(80u16, 30u16), (20, 8), (3, 3)] {
            for focus in [Focus::Table, Focus::Log, Focus::Activity] {
                for expanded in [false, true] {
                    for filter in [None, Some("line 4".to_string())] {
                        for act_sel in [0, usize::MAX] {
                            for log_off in [0, 1000] {
                                for input in [None, Some("abc")] {
                                    let browse = BrowseState {
                                        focus,
                                        act_expanded: expanded,
                                        filter: filter.clone(),
                                        act_sel,
                                        log_off,
                                        ..BrowseState::default()
                                    };
                                    let mut table_state = TableState::default();
                                    table_state.select(Some(0));
                                    let mut terminal =
                                        Terminal::new(TestBackend::new(w, h)).unwrap();
                                    terminal
                                        .draw(|f| {
                                            render(f, &state, &mut table_state, input, &browse);
                                        })
                                        .unwrap();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_run_renders_without_panicking() {
        let state = TuiState::new();
        let browse = BrowseState {
            focus: Focus::Activity,
            act_expanded: true,
            act_sel: usize::MAX,
            ..BrowseState::default()
        };
        let mut table_state = TableState::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal
            .draw(|f| {
                render(f, &state, &mut table_state, None, &browse);
            })
            .unwrap();
    }
}
