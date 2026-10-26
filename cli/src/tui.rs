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

use std::collections::{HashMap, VecDeque};
use std::io::{self, IsTerminal};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;
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
        Block, BorderType, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Wrap,
    },
};

use ap_core::event::{Event, PackageOutcome, RunOutcome, SkipReason, StepKind, StepOutcome, Task};
use ap_core::sink::{MemorySink, Reporter};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_LOG_LINES: usize = 300;
const MAX_AGENT_ACTIVITY: usize = 100;

/// Bound on events queued for the render thread; beyond it events are dropped.
const EVENT_QUEUE_CAP: usize = 8192;

/// What a run is about, shown in the summary header: the OBS project it
/// builds in and the packages it was asked for. No `targets` means the whole
/// project.
#[derive(Clone, Default)]
pub struct RunScope {
    pub project: String,
    pub targets: Vec<String>,
}

/// Handle used by the CLI to obtain reporters that feed the TUI.
///
/// Cheap to clone; every clone shares the same channel to the render thread.
#[derive(Clone)]
pub struct TuiController {
    tx: SyncSender<Msg>,
    /// Events dropped because the render thread's queue was full.
    dropped: std::sync::Arc<AtomicUsize>,
    /// Package requests from the TUI's `n` prompt and `r` key. Handed out
    /// once, via [`TuiController::take_add_requests`]; disconnects when the
    /// render thread exits, which is how the consumer learns the user quit.
    /// Only a run that accepts requests has `n` and `r` keys (see
    /// [`TuiController::new`]); for any other run nothing is ever sent.
    add_rx: std::sync::Arc<std::sync::Mutex<Option<Receiver<AddRequest>>>>,
    /// The name each row was requested under, keyed by the row's RPM name.
    /// Filled by the build (see [`TuiController::note_origin`]) and read when
    /// a row is retried, since the RPM name is not what a build takes.
    origins: std::sync::Arc<Mutex<HashMap<String, String>>>,
    /// The error that ended the render loop early, if it did (see
    /// [`TuiController::failure`]).
    failure: std::sync::Arc<Mutex<Option<String>>>,
    /// Builds the run still owes: the root build until
    /// [`TuiController::work_finished`] is called for it, and each add request
    /// or retry from the time the TUI sends it until its build returns. The
    /// TUI closes itself only once this is zero (see [`run_is_done`]).
    work: std::sync::Arc<AtomicUsize>,
    /// Set by the render thread just before it returns: whether every package
    /// was built when it closed (see [`TuiController::all_settled`]).
    settled: std::sync::Arc<AtomicBool>,
    /// Lets [`TuiController::work_finished`] wake a waiter in
    /// [`TuiController::wait_until_idle`] once the work reaches zero.
    idle: std::sync::Arc<(Mutex<()>, Condvar)>,
    /// The render thread, taken by [`TuiController::wait_closed`].
    render: std::sync::Arc<Mutex<Option<JoinHandle<()>>>>,
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

/// The terminal the render thread draws on, and the guard that restores it.
struct Screen {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    _restore: TerminalGuard,
}

impl Screen {
    /// Takes over the terminal, or fails having changed nothing that needs
    /// undoing. Stdout must be a terminal: raw mode can succeed on
    /// `/dev/tty` even when stdout is redirected, and then the TUI would
    /// draw into a file. The guard is armed only once raw mode is on, so a
    /// failure before that writes no escape sequences.
    fn open() -> io::Result<Self> {
        if !io::stdout().is_terminal() {
            return Err(io::Error::other("stdout is not a terminal"));
        }
        enable_raw_mode()?;
        let restore = TerminalGuard::new();
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        // Without this, the alternate screen keeps whatever happened to be in its
        // buffer when we switched to it (shell scrollback on terminals that mirror
        // the primary buffer, previous-run leftovers on terminals that preserve
        // the alt buffer across invocations). Ratatui's `draw` only sends diffs
        // against its own previous buffer — not the terminal's — so cells we
        // render as default (space + default style) silently keep that leftover
        // content showing through. One hard clear up front wipes the slate.
        terminal.clear()?;
        Ok(Self {
            terminal,
            _restore: restore,
        })
    }
}

/// A request for a new root build, sent to the run by the TUI.
pub enum AddRequest {
    /// A package name or repository URL typed into the `n` prompt.
    New(String),
    /// A retry of the row with this key (its RPM name), sent under the name
    /// the row was requested with (see [`TuiController::origin_of`]).
    Retry(String),
}

struct Msg {
    pkg: Option<String>,
    event: Event,
}

impl TuiController {
    /// Take over the terminal, spawn the render thread, and return a handle to
    /// feed it events.
    ///
    /// Fails, with the terminal left as it was, when the TUI cannot be shown
    /// (stdout is not a terminal, or raw mode or the alternate screen is
    /// refused). The caller then runs without it, as a plain run.
    ///
    /// `memory` is only used for a Ctrl-C keypress inside the TUI — see
    /// `crate::handle_interrupt` for why the TUI cannot just rely on the
    /// process's real SIGINT handler firing. `scope` is only shown in the
    /// summary header. `accepts_requests` says whether the run has a consumer
    /// for the `n` and `r` keys (see [`TuiController::take_add_requests`]);
    /// when it does not, those keys do nothing and are not shown.
    pub fn new(
        memory: std::sync::Arc<MemorySink>,
        scope: RunScope,
        accepts_requests: bool,
    ) -> io::Result<Self> {
        let (tx, rx) = sync_channel(EVENT_QUEUE_CAP);
        let (add_tx, add_rx) = channel();
        // Starts at one for the root build, which the CLI releases when it returns.
        let work = std::sync::Arc::new(AtomicUsize::new(1));
        let thread_work = work.clone();
        let settled = std::sync::Arc::new(AtomicBool::new(false));
        let thread_settled = settled.clone();
        let failure = std::sync::Arc::new(Mutex::new(None));
        let thread_failure = failure.clone();
        let add_tx = accepts_requests.then_some(add_tx);
        // The render thread reports whether it could take the terminal over
        // before it does anything else, so a failure comes back here, with
        // nothing printed yet.
        let (ready_tx, ready_rx) = sync_channel::<io::Result<()>>(1);
        let render = std::thread::spawn(move || {
            let screen = match Screen::open() {
                Ok(screen) => screen,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            if let Err(e) = run(
                screen,
                rx,
                add_tx,
                thread_work,
                memory,
                scope,
                thread_settled,
            ) {
                eprintln!("TUI error: {e}");
                *thread_failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
            }
        });
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = render.join();
                return Err(e);
            }
            Err(_) => {
                let _ = render.join();
                return Err(io::Error::other("the TUI thread ended before it started"));
            }
        }
        Ok(Self {
            tx,
            dropped: std::sync::Arc::new(AtomicUsize::new(0)),
            add_rx: std::sync::Arc::new(std::sync::Mutex::new(Some(add_rx))),
            origins: std::sync::Arc::new(Mutex::new(HashMap::new())),
            failure,
            work,
            settled,
            idle: std::sync::Arc::new((Mutex::new(()), Condvar::new())),
            render: std::sync::Arc::new(Mutex::new(Some(render))),
        })
    }

    /// The error that ended the render loop before the run closed, if it did.
    /// `None` when the TUI closed on its own terms, whether or not the run
    /// settled. Read it after [`TuiController::wait_closed`].
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Record the name `row` (an RPM name) was requested under, so a retry
    /// can resend that name. The first name recorded for a row is kept.
    pub fn note_origin(&self, row: &str, request: &str) {
        self.origins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(row.to_string())
            .or_insert_with(|| request.to_string());
    }

    /// The name `row` was requested under, if one was recorded. Rows a
    /// build did not start from a request (dependencies, for one) have none.
    pub fn origin_of(&self, row: &str) -> Option<String> {
        self.origins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(row)
            .cloned()
    }

    /// Whether the TUI closed with every package built (see [`run_is_done`]).
    /// The render thread stores this before it returns, so once the CLI has
    /// waited for the TUI to close (see [`TuiController::wait_closed`]) this
    /// reads the final verdict. It stays `false` if the TUI never closed
    /// normally.
    pub fn all_settled(&self) -> bool {
        self.settled.load(Ordering::SeqCst)
    }

    /// Release one unit of outstanding work: the root build once it returns,
    /// or an add request once its build returns. Call it only after the
    /// build's last event has been reported, so the TUI never sees the run as
    /// finished while events are still on their way.
    pub fn work_finished(&self) {
        if self.work.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Taking the lock first means a waiter that has just seen the work
            // outstanding is already parked on the condition when this notifies.
            let _guard = self.idle.0.lock().unwrap_or_else(|p| p.into_inner());
            self.idle.1.notify_all();
        }
    }

    /// Block until no work is outstanding (see [`TuiController::work_finished`]).
    /// Work the TUI adds while waiting is waited for too, so this returns at
    /// the first moment the run has nothing left in flight. It does not wait
    /// for the TUI to close, so it is safe to call before the add-request
    /// dispatcher has ended.
    pub fn wait_until_idle(&self) {
        let (lock, cvar) = &*self.idle;
        let mut guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        while self.work.load(Ordering::SeqCst) != 0 {
            guard = cvar.wait(guard).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Block until the TUI has closed: its render thread has restored the
    /// terminal and returned. Returns at once if it already has, or if it
    /// was never started or has been waited for before.
    pub fn wait_closed(&self) {
        let handle = self.render.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Take the receiver for package names entered at runtime (`n` key).
    /// `None` after the first call. `recv` fails once the TUI has exited.
    pub fn take_add_requests(&self) -> Option<Receiver<AddRequest>> {
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
        let msg = Msg {
            pkg: self.pkg.clone(),
            event: event.clone(),
        };
        // Lifecycle events move a row to its next state. Dropping one leaves the
        // row stale for the rest of the run, so they wait for room. Progress
        // events may be dropped: the run log has them all.
        if ap_core::sink::is_lifecycle(event) {
            let _ = self.tx.send(msg);
            return;
        }
        // Never block a reporter on a slow render thread for progress events.
        match self.tx.try_send(msg) {
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
            PkgStatus::MissingDeps => "blocked",
            PkgStatus::Failed => "failed",
            PkgStatus::NoSpace => "no space",
        }
    }

    /// A one-column symbol in front of the label, so a row's state reads at a
    /// glance and still shows without colour. `tick` animates the running
    /// symbol.
    fn glyph(&self, tick: u64) -> &'static str {
        match self {
            PkgStatus::Queued => "○",
            PkgStatus::Running => ["◐", "◓", "◑", "◒"][(tick % 4) as usize],
            PkgStatus::Between => "◌",
            PkgStatus::Succeeded => "✓",
            PkgStatus::AlreadyBuilt => "✔",
            PkgStatus::MissingDeps => "◇",
            PkgStatus::Failed | PkgStatus::NoSpace => "✗",
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

/// Longest suffix of `s` whose display width fits in `max_width` columns.
fn suffix_within_width(s: &str, max_width: usize) -> &str {
    let mut used = 0;
    let mut start = s.len();
    for (idx, c) in s.char_indices().rev() {
        let width = c.width().unwrap_or(0);
        if used + width > max_width {
            break;
        }
        used += width;
        start = idx;
    }
    &s[start..]
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
    log_lines: VecDeque<LogLine>,
    /// Lines ever added, including ones since evicted from `log_lines`. Only
    /// grows, so a view can count arrivals even once the cap is hit.
    log_seq: u64,
    agent_activity: VecDeque<ActivityEntry>,
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

    fn add_agent_activity(&mut self, kind: ActivityKind, text: String) {
        self.agent_activity.push_back(ActivityEntry { kind, text });
        self.activity_seq += 1;
        if self.agent_activity.len() > MAX_AGENT_ACTIVITY {
            self.agent_activity.pop_front();
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
    /// Project and requested packages, shown in the summary header.
    scope: RunScope,
    order: Vec<String>,
    packages: HashMap<String, PkgState>,
    // Only the most recent run-level message is ever shown (the footer),
    // so there is nothing to gain from keeping a bounded history here the
    // way per-package logs do.
    last_global_log: Option<LogLine>,
    /// Run-level messages, newest last: what the Event Log shows while no
    /// package is selected, such as the sweep that plans an `update` before
    /// any package has a row.
    run_log: VecDeque<LogLine>,
    run_outcome: Option<RunOutcome>,
    /// Whether the run takes the `n` and `r` keys (see `TuiController::new`).
    accepts_requests: bool,
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
    fn new(scope: RunScope) -> Self {
        Self {
            scope,
            order: Vec::new(),
            packages: HashMap::new(),
            last_global_log: None,
            run_log: VecDeque::new(),
            run_outcome: None,
            accepts_requests: false,
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
        let line = LogLine {
            level,
            message,
            emphasis: false,
        };
        self.run_log.push_back(line.clone());
        if self.run_log.len() > MAX_LOG_LINES {
            self.run_log.pop_front();
        }
        self.last_global_log = Some(line);
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

    /// Packages still to build: rows that are neither finished nor parked on
    /// missing deps after a failure. Derived on every call, like `done`, so it
    /// cannot drift from the rows the way a stored counter could.
    fn pending_count(&self) -> usize {
        self.packages
            .values()
            .filter(|p| !p.finished && p.status != PkgStatus::MissingDeps)
            .count()
    }

    fn pkg_mut(&mut self, name: &str) -> &mut PkgState {
        self.packages.entry(name.to_string()).or_insert_with(|| {
            self.order.push(name.to_string());
            PkgState::new(String::new())
        })
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
                        "Resolved: {total} total, {built} built, {in_distro} in the distro; {remaining} were still to build when resolved"
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
            // Run-level progress, from before any package has a row.
            Event::Progress { message, .. } if msg.pkg.is_none() => {
                self.add_global_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } if msg.pkg.is_none() => {
                self.add_global_log(LogLevel::Warn, message.clone());
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
                    Task::PatchDependency => "Patching a dependency".to_string(),
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

/// Runs the event loop on an open `screen`. The screen's guard restores the
/// terminal on every way out of this function — clean exit, a render or input
/// error, or a panic unwinding through here. (Ctrl-C is the one exit path that
/// does NOT come back through here at all — `crate::handle_interrupt` does its
/// own equivalent teardown and then ends the process on the spot.)
fn run(
    mut screen: Screen,
    rx: Receiver<Msg>,
    add_tx: Option<Sender<AddRequest>>,
    work: std::sync::Arc<AtomicUsize>,
    memory: std::sync::Arc<MemorySink>,
    scope: RunScope,
    settled: std::sync::Arc<AtomicBool>,
) -> io::Result<()> {
    let result = event_loop(
        &mut screen.terminal,
        rx,
        add_tx.as_ref(),
        &work,
        &memory,
        scope,
        &settled,
    );
    let _ = screen.terminal.show_cursor();
    result
}

/// Whether every package the run was asked for is settled, so the TUI can
/// close by itself and the run exits 0.
///
/// A package is settled once its own build has finished with a built outcome
/// (`Built` or `AlreadyBuilt`) and no retry is asked for. Nothing else settles
/// a row: one parked on missing deps is not settled, even once its dependencies
/// are built, and neither is a failed row.
///
/// `work_left` counts the builds still running (see
/// [`TuiController::work_finished`]), so a run with work outstanding is never
/// settled. Rows are all checked rather than only the scope's targets, since
/// the targets are typed names and rows use rpm names, so matching them up is
/// not reliable.
///
/// A run with no rows at all is settled only once it has ended successfully:
/// `RunFinished` is reported after the root work is released, so until then a
/// `build` that has not shown its first row yet stays open, while an `update`
/// with nothing to do closes at once. A failure before anything started stays
/// on screen.
fn run_is_done(state: &TuiState, work_left: usize) -> bool {
    if work_left != 0 {
        return false;
    }
    if state.packages.is_empty() {
        return matches!(state.run_outcome, Some(RunOutcome::Success));
    }
    state.packages.values().all(|p| {
        p.finished
            && !p.retry_requested
            && matches!(p.status, PkgStatus::Succeeded | PkgStatus::AlreadyBuilt)
    })
}

/// Whether the run has nothing left to do, built or not: `work_left` is zero
/// and every row has finished with no retry asked for. Such a run is over but
/// may not be settled (see [`run_is_done`]); then the TUI stays open so the
/// user can add a package, retry one, or quit. A row parked on missing deps
/// counts as finished here, since its attempt has ended and nothing will
/// re-queue it once `work_left` is zero.
fn run_is_over(state: &TuiState, work_left: usize) -> bool {
    work_left == 0
        && !state.packages.is_empty()
        && state
            .packages
            .values()
            .all(|p| p.finished && !p.retry_requested)
}

/// The footer line for a run that is over but not settled, so the user knows
/// what to do next. `None` while the run is still going and once it is
/// settled, since the TUI is about to close itself then. Only a run that
/// accepts requests offers `n`.
fn over_hint(over: bool, settled: bool, accepts_requests: bool) -> Option<&'static str> {
    if !over || settled {
        return None;
    }
    Some(if accepts_requests {
        "Run over with packages not built: press n to add a package, q to quit"
    } else {
        "Run over with packages not built: press q to quit"
    })
}

/// Hand `name` to the run as a new root build. It is counted as outstanding
/// work before it is sent, so the run cannot look finished in the gap between
/// the request leaving and its build starting. `false` if the run is gone.
fn request_build(add_tx: &Sender<AddRequest>, work: &AtomicUsize, req: AddRequest) -> bool {
    work.fetch_add(1, Ordering::SeqCst);
    let sent = add_tx.send(req).is_ok();
    if !sent {
        work.fetch_sub(1, Ordering::SeqCst);
    }
    sent
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: Receiver<Msg>,
    add_tx: Option<&Sender<AddRequest>>,
    work: &AtomicUsize,
    memory: &MemorySink,
    scope: RunScope,
    settled: &AtomicBool,
) -> io::Result<()> {
    let mut state = TuiState::new(scope);
    state.accepts_requests = add_tx.is_some();
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
    // Read fresh at each use: a key press can add work partway through a frame.
    let run_over = |state: &TuiState| {
        state.run_outcome.is_some() || run_is_over(state, work.load(Ordering::SeqCst))
    };

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

        let hint = over_hint(
            run_over(&state),
            run_is_done(&state, work.load(Ordering::SeqCst)),
            state.accepts_requests,
        );
        let mut feedback = RenderFeedback::default();
        terminal.draw(|f| {
            feedback = render(f, &state, &mut table_state, input.as_deref(), &browse, hint);
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
                        if add_tx.is_some_and(|tx| {
                            request_build(tx, work, AddRequest::New(name.clone()))
                        }) {
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
                KeyCode::Char('n') | KeyCode::Char('+') if add_tx.is_some() => {
                    input = Some(String::new())
                }
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
                // Once the run is over there is nothing to interrupt: leave
                // normally, so a successful run exits 0 and a failed one keeps
                // the exit code the CLI gives it.
                KeyCode::Char('q') | KeyCode::Esc => {
                    if run_over(&state) {
                        break;
                    }
                    crate::handle_interrupt(true, memory)
                }
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    if run_over(&state) {
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
                    // Each list clamps against its own length, so the shorter
                    // one stops at its end while the longer keeps scrolling.
                    browse.queued_off = text_scroll_key(
                        key.code,
                        browse.queued_off,
                        feedback.queued_total,
                        feedback.lists_viewport,
                    );
                    browse.built_off = text_scroll_key(
                        key.code,
                        browse.built_off,
                        feedback.built_total,
                        feedback.lists_viewport,
                    );
                }
                // Retry a package that failed with "No space left on
                // device" (`PkgStatus::NoSpace`) once the user has freed
                // some — re-sent through the same channel as the `n` add
                // prompt, as a retry of this row. Any other status is a
                // no-op: there is nothing a disk cleanup would fix for a
                // real spec defect.
                KeyCode::Char('r') => {
                    if let Some(add_tx) = add_tx
                        && let Some(name) = selected.as_ref()
                        && state
                            .packages
                            .get(name.as_str())
                            .is_some_and(|p| p.status == PkgStatus::NoSpace && !p.retry_requested)
                    {
                        if request_build(add_tx, work, AddRequest::Retry(name.clone())) {
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

        // A settled run gets a two-second grace so its final state can be read.
        // Work added during the grace (`n` or `r`) cancels it, and the timer
        // starts over once the run is settled again. A run that is over but not
        // settled stays open until the user quits.
        if run_is_done(&state, work.load(Ordering::SeqCst)) {
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
        } else {
            finished_at = None;
        }
    }

    settled.store(
        run_is_done(&state, work.load(Ordering::SeqCst)),
        Ordering::SeqCst,
    );
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
    /// First line shown of each list; separate because the lists differ in
    /// length and a shared offset would end the shorter one early.
    queued_off: usize,
    built_off: usize,
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
    /// Length of each list and how many lines fit, for scrolling them.
    queued_total: usize,
    built_total: usize,
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
fn filtered_indices<'a>(lines: impl IntoIterator<Item = &'a LogLine>, needle: &str) -> Vec<usize> {
    lines
        .into_iter()
        .enumerate()
        .filter(|(_, l)| filter_matches(&l.message, needle))
        .map(|(i, _)| i)
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
    hint: Option<&str>,
) -> RenderFeedback {
    let area = f.area();
    if too_small(area) {
        render_too_small(f, area);
        return RenderFeedback::default();
    }
    let visible = state.visible_order();
    let heights = stacked_heights(area.height, table_wanted(visible.len()));
    let [summary, table, detail, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints(heights.map(Constraint::Length))
        .areas(area);

    render_summary(f, state, summary);
    render_table(
        f,
        state,
        &visible,
        table,
        table_state,
        browse.focus == Focus::Table,
    );
    // The run's own log has a pane of its own below the package panes, so it
    // never shares one with a package's log.
    let (detail, run) = split_run_pane(detail, !state.run_log.is_empty());
    if let Some(run) = run {
        render_run_log(f, state, run);
    }
    let feedback = render_detail(f, state, &visible, detail, table_state.selected(), browse);
    let retryable = state.accepts_requests
        && table_state
            .selected()
            .and_then(|i| visible.get(i))
            .and_then(|name| state.packages.get(name.as_str()))
            .is_some_and(|p| p.status == PkgStatus::NoSpace && !p.retry_requested);
    render_footer(f, state, input, browse, footer, retryable, hint);
    feedback
}

/// Rows of the run log pane at most, borders included.
const RUN_PANE_MAX_HEIGHT: u16 = 7;
/// Rows of the detail area below which the run log gets no pane, so the
/// package panes are never squeezed for it.
const RUN_PANE_MIN_DETAIL: u16 = 14;

/// Carves the run log pane off the bottom of the detail `area`, when there is
/// something to show and room for it beside the package panes.
fn split_run_pane(area: Rect, has_log: bool) -> (Rect, Option<Rect>) {
    if !has_log || area.height < RUN_PANE_MIN_DETAIL {
        return (area, None);
    }
    let height = (area.height / 4).clamp(4, RUN_PANE_MAX_HEIGHT);
    let [detail, run] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(height)])
        .areas(area);
    (detail, Some(run))
}

/// The newest run-level messages: the sweep that plans an `update` or audits
/// a project, which goes on while packages are being built.
fn render_run_log(f: &mut Frame, state: &TuiState, area: Rect) {
    let tail = window(state.run_log.len(), viewport_height(area.height), 0);
    let items: Vec<ListItem> = state
        .run_log
        .range(tail)
        .map(|l| ListItem::new(l.message.as_str()).style(Style::default().fg(l.level.color())))
        .collect();
    f.render_widget(List::new(items).block(pane("Run", false)), area);
}

/// Below this size nothing readable fits, so only a notice is drawn.
const MIN_WIDTH: u16 = 20;
const MIN_HEIGHT: u16 = 6;
/// Rows from which the summary and footer keep their bordered three-row
/// boxes; shorter terminals get them as one bare line each.
const FULL_CHROME_MIN_HEIGHT: u16 = 20;
/// Width from which two panes sit side by side; narrower ones are stacked.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 60;

fn too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// The one line drawn instead of the UI when the terminal is too small.
fn render_too_small(f: &mut Frame, area: Rect) {
    let line = truncate_with_ellipsis("terminal too small", area.width as usize);
    let row = Rect {
        height: area.height.min(1),
        ..area
    };
    f.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::Red)),
        row,
    );
}

/// Rows the package table wants: its header and border, plus one per package,
/// within a sensible range.
fn table_wanted(rows: usize) -> u16 {
    u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(3)
        .clamp(5, 12)
}

/// Heights of the summary, package table, detail pane and footer, top to
/// bottom, for a terminal `height` rows tall. The summary and footer shrink
/// to a single bare line when the terminal is short. The table takes what it
/// wants up to half of the body, and the detail pane keeps the rest, so
/// neither is squeezed out by the other.
fn stacked_heights(height: u16, table_wanted: u16) -> [u16; 4] {
    let chrome = if height >= FULL_CHROME_MIN_HEIGHT {
        3
    } else {
        1
    };
    let body = height.saturating_sub(2 * chrome);
    let table = table_wanted.min(body / 2);
    [chrome, table, body - table, chrome]
}

/// Splits `area` into two panes: side by side when the width allows, stacked
/// otherwise. `first` is the percentage of the space the first pane gets.
fn two_panes(area: Rect, first: u16) -> [Rect; 2] {
    let direction = if area.width >= SIDE_BY_SIDE_MIN_WIDTH {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    Layout::default()
        .direction(direction)
        .constraints([
            Constraint::Percentage(first),
            Constraint::Percentage(100 - first),
        ])
        .areas(area)
}

/// The first of `options` at most `width` columns wide, or the last option
/// when none fits. Options are listed from the fullest to the most cut down.
fn first_fitting<T>(options: Vec<T>, width: usize, measure: impl Fn(&T) -> usize) -> Option<T> {
    let mut last = None;
    for option in options {
        if measure(&option) <= width {
            return Some(option);
        }
        last = Some(option);
    }
    last
}

/// Columns left inside a bordered pane for its content.
fn pane_inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

/// Whether a one-line strip is drawn in a box. Short terminals get the
/// summary and footer as bare lines instead.
fn is_boxed(area: Rect) -> bool {
    area.height >= 3
}

/// Columns of text that fit in a one-line strip, leaving out its border.
fn text_width(area: Rect) -> usize {
    if is_boxed(area) {
        pane_inner_width(area)
    } else {
        area.width as usize
    }
}

/// `p` inside a border, when `area` is tall enough to hold one.
fn framed(area: Rect, p: Paragraph<'_>) -> Paragraph<'_> {
    if is_boxed(area) {
        p.block(chrome_block())
    } else {
        p
    }
}

/// A bordered pane. The focused one gets a yellow border and a `▶` in its
/// title, so focus still shows without colour.
fn pane(title: &str, focused: bool) -> Block<'static> {
    let block = chrome_block();
    if focused {
        block
            .border_style(Style::default().fg(Color::Yellow))
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .title(format!("▶ {title}"))
    } else {
        block.title(title.to_string())
    }
}

/// The bordered box every pane and strip is drawn in: rounded, with a dim
/// border so the content, not the frame, carries the eye.
fn chrome_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

/// The run's scope as text, fullest first: every target, then the first few
/// with `(+N)` for the rest, then the project alone. Without targets the
/// whole project is the scope.
fn scope_labels(scope: &RunScope) -> Vec<String> {
    let project = format!("project: {}", scope.project);
    let total = scope.targets.len();
    if total == 0 {
        return vec![format!("{project} | target: all packages"), project];
    }
    let mut labels: Vec<String> = (1..=total)
        .rev()
        .map(|shown| {
            let names = scope.targets[..shown].join(", ");
            let more = if shown < total {
                format!(" (+{})", total - shown)
            } else {
                String::new()
            };
            format!("{project} | target: {names}{more}")
        })
        .collect();
    labels.push(project);
    labels
}

/// The summary line: the scope in front of the counters. Every combination is
/// tried with the scope first, so the project and targets keep their place
/// while there is room for them beside the counters, and the counters shrink
/// before the scope does. Counters alone come last, for terminals too narrow
/// for both. Whatever is picked is cut to `width` as a last resort.
fn header_line(scope: &[String], counters: Vec<String>, width: usize) -> String {
    let mut options: Vec<String> = scope
        .iter()
        .flat_map(|s| counters.iter().map(move |c| format!("{s} │{c}")))
        .collect();
    options.extend(counters);
    let line = first_fitting(options, width, |t| t.width()).unwrap_or_default();
    truncate_with_ellipsis(&line, width)
}

fn render_summary(f: &mut Frame, state: &TuiState, area: Rect) {
    let done = state.packages.values().filter(|p| p.finished).count();
    let total = state.order.len();
    let pending = state.pending_count();
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
    let cost = state.total_cost_usd();
    let full = match (state.resolve_total, state.resolve_in_distro) {
        (Some(closure_total), Some(in_distro)) => format!(
            " Auto-Package — {done}/{total} packages done, {pending} pending ({built} built, {in_distro}/{closure_total} in distro) │ elapsed {elapsed}s │ cost ${cost:.4} "
        ),
        _ => format!(
            " Auto-Package — {done}/{total} packages done, {pending} pending ({built} built) │ elapsed {elapsed}s │ cost ${cost:.4} "
        ),
    };
    // Narrower titles for narrower terminals, each dropping the least
    // essential parts first.
    let short =
        format!(" {done}/{total} done, {pending} pending │ elapsed {elapsed}s │ ${cost:.4} ");
    let shortest = format!(" {done}/{total} done │ ${cost:.4} ");
    let title = header_line(
        &scope_labels(&state.scope),
        vec![full, short, shortest],
        text_width(area),
    );
    // The `│` dividers are dimmed so the scope and each counter group stand
    // apart; the text itself is unchanged.
    let strong = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for (i, part) in title.split('│').enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(part.to_string(), strong));
    }
    let header = Paragraph::new(Line::from(spans));
    f.render_widget(framed(area, header), area);
}

/// Package table columns as (header, fixed width), in display order. A `None`
/// width is a flexible column that takes whatever the others leave.
const TABLE_COLUMNS: [(&str, Option<u16>); 7] = [
    ("Status", Some(11)),
    ("Package", None),
    ("Version", Some(12)),
    ("Step", None),
    ("Tries", Some(7)),
    ("Time", Some(7)),
    ("Cost", Some(9)),
];
/// Column indices by importance, most important first. On a narrow terminal
/// the last ones are dropped first.
const TABLE_PRIORITY: [usize; 7] = [1, 0, 3, 5, 2, 4, 6];
/// Least width a flexible column needs before it is worth showing.
const FLEX_MIN_WIDTH: u16 = 12;

/// Indices into `TABLE_COLUMNS` of the columns a table `width` wide shows, in
/// display order. Columns are added in priority order while they still fit,
/// and the package name is always shown.
fn visible_columns(width: u16) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for col in TABLE_PRIORITY {
        let mut trial = kept.clone();
        trial.push(col);
        trial.sort_unstable();
        if kept.is_empty() || table_needs(&trial) <= width {
            kept = trial;
        }
    }
    kept
}

/// Width a table showing `columns` needs: each fixed column's width, a
/// minimum for each flexible one, a space between columns and the borders.
fn table_needs(columns: &[usize]) -> u16 {
    let spaces = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(u16::MAX);
    columns.iter().fold(spaces.saturating_add(2), |need, &i| {
        need.saturating_add(TABLE_COLUMNS[i].1.unwrap_or(FLEX_MIN_WIDTH))
    })
}

fn render_table(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    table_state: &mut TableState,
    focused: bool,
) {
    let columns = visible_columns(area.width);
    let spin = state
        .run_start
        .map_or(0, |s| s.elapsed().as_millis() as u64 / 250);
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
            let cells = [
                Cell::from(format!("{} {}", pkg.status.glyph(spin), pkg.status.label()))
                    .style(Style::default().fg(pkg.status.color())),
                Cell::from(name.as_str()).style(
                    if matches!(pkg.status, PkgStatus::Queued | PkgStatus::AlreadyBuilt) {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    },
                ),
                Cell::from(pkg.version.as_str()).style(Style::default().fg(Color::Gray)),
                Cell::from(step),
                Cell::from(tries),
                Cell::from(format!("{elapsed}s")),
                Cell::from(format!("${:.4}", pkg.cost_usd)),
            ];
            let kept = cells
                .into_iter()
                .enumerate()
                .filter(|(i, _)| columns.contains(i))
                .map(|(_, cell)| cell);
            Row::new(kept)
        })
        .collect();

    let widths = columns.iter().map(|&i| match TABLE_COLUMNS[i].1 {
        Some(width) => Constraint::Length(width),
        None => Constraint::Fill(1),
    });
    let titles = TABLE_COLUMNS
        .iter()
        .enumerate()
        .filter(|(i, _)| columns.contains(i))
        .map(|(_, (title, _))| *title);

    let table = Table::new(rows, widths)
        .header(
            Row::new(titles).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
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
                    .block(
                        chrome_block()
                            .border_style(Style::default().fg(Color::Red))
                            .title_style(
                                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                            )
                            .title(format!("✗ {title}")),
                    ),
                rows[0],
            );
            rows[1]
        }
        None => area,
    };
    let [log_area, act_area] = two_panes(body, 65);

    let Some(pkg) = pkg else {
        f.render_widget(
            Paragraph::new("No package selected yet")
                .block(pane("Event Log", browse.focus == Focus::Log)),
            log_area,
        );
        f.render_widget(chrome_block().title("Agent Activity"), act_area);
        return RenderFeedback::default();
    };

    let log_viewport = viewport_height(log_area.height);
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
            ListItem::new(l.message.as_str()).style(style)
        })
        .collect();
    let filter_note = browse
        .filter
        .as_deref()
        .map(|needle| format!(" [filter: {needle}]"))
        .unwrap_or_default();
    let (more_note, short_more_note) = if scroll > 0 {
        (
            format!(" — {scroll} more below, End to follow"),
            format!(" ↓{scroll}"),
        )
    } else {
        (String::new(), String::new())
    };
    let skipped_note = if pkg.skipped.is_empty() {
        String::new()
    } else {
        let labels: Vec<&str> = pkg.skipped.iter().map(|(k, _)| k.label()).collect();
        format!(" (skipped: {})", labels.join(", "))
    };
    let log_title = first_fitting(
        vec![
            format!("Event Log{filter_note}{more_note}{skipped_note}"),
            format!("Event Log{filter_note}{more_note}"),
            format!("Event Log{filter_note}{short_more_note}"),
        ],
        pane_inner_width(log_area),
        |t| t.width(),
    )
    .unwrap_or_default();
    let log = List::new(log_items).block(pane(&log_title, browse.focus == Focus::Log));

    let act_viewport = viewport_height(act_area.height);
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
        let text_viewport = viewport_height(log_area.height);
        let text = wrap_text(&entry.text, pane_inner_width(log_area));
        let off = clamp_offset(browse.detail_off, text.len(), text_viewport);
        let end = (off + text_viewport).min(text.len());
        let lines: Vec<Line> = text[off..end]
            .iter()
            .map(|l| Line::styled(l.as_str(), Style::default().fg(entry.kind.color())))
            .collect();
        let title = format!("{} — Esc close, ↑/↓ scroll", entry.kind.label());
        f.render_widget(
            Paragraph::new(lines).block(pane(&title, browse.focus == Focus::Activity)),
            log_area,
        );
        feedback.detail_total = text.len();
        feedback.detail_viewport = text_viewport;
        feedback.detail_off = off;
    } else {
        f.render_widget(log, log_area);
    }

    let inner_w = pane_inner_width(act_area);
    let activity_items: Vec<ListItem> = pkg
        .agent_activity
        .iter()
        .skip(act_top)
        .take(act_viewport)
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
    let position = if act_len > 0 {
        format!(" {}/{act_len}", act_sel + 1)
    } else {
        String::new()
    };
    let tokens = format!("{} in / {} out", pkg.input_tokens, pkg.output_tokens);
    let activity_title = first_fitting(
        vec![
            format!("Agent Activity{position} — {tokens}"),
            format!("Agent Activity{position}"),
            "Activity".to_string(),
        ],
        pane_inner_width(act_area),
        |t| t.width(),
    )
    .unwrap_or_default();
    let activity = List::new(activity_items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(pane(&activity_title, browse.focus == Focus::Activity));
    f.render_stateful_widget(activity, act_area, &mut list_state);
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
                let deps: Vec<String> = p
                    .waiting_on
                    .iter()
                    .map(|dep| waiting_label(state, dep))
                    .collect();
                format!("{n} <- {}", deps.join(", "))
            }
        })
        .collect()
}

/// A dependency a package waits on, as the table knows it: the same package
/// has a row under its RPM name (`python-logfire` for `logfire`), so without
/// its status here it would read as a second entry for one package.
fn waiting_label(state: &TuiState, dep: &str) -> String {
    let key = ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep));
    let row = state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .find(|(n, _)| ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n)) == key);
    match row {
        Some((_, p)) => format!("{dep} ({})", p.status.label()),
        None => dep.to_string(),
    }
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

/// The Queued and Built lists side by side (stacked on a narrow terminal) in
/// place of the detail panes. One offset is kept per list and clamped to that
/// list's own length.
fn render_lists(
    f: &mut Frame,
    state: &TuiState,
    area: Rect,
    browse: &BrowseState,
) -> RenderFeedback {
    let queued = queued_lines(state);
    let built = built_lines(state);
    let [queued_area, built_area] = two_panes(area, 50);
    // Both lists page by the same amount, the smaller pane's height, so one
    // offset per list means the same thing whichever list is scrolled.
    let viewport = viewport_height(queued_area.height).min(viewport_height(built_area.height));
    for (lines, title, col, off) in [
        (
            &queued,
            format!("Queued ({})", queued.len()),
            queued_area,
            clamp_offset(browse.queued_off, queued.len(), viewport),
        ),
        (
            &built,
            format!("Built ({})", built.len()),
            built_area,
            clamp_offset(browse.built_off, built.len(), viewport),
        ),
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
        queued_total: queued.len(),
        built_total: built.len(),
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
    hint: Option<&str>,
) {
    let prompt = match (input, browse.filter_input.as_deref()) {
        (Some(buf), _) => Some((
            "add package (name or repo URL): ",
            "add: ",
            buf,
            "   Enter queue │ Esc cancel",
        )),
        (None, Some(buf)) => Some((
            "filter log: ",
            "filter: ",
            buf,
            "   Enter apply │ Esc cancel",
        )),
        (None, None) => None,
    };
    if let Some((label, short_label, buf, keys)) = prompt {
        // The cursor must stay on screen, so when even the short label does
        // not fit, only the end of what was typed is shown.
        let width = text_width(area);
        let line = first_fitting(
            vec![
                prompt_line(label, buf, keys),
                prompt_line(label, buf, ""),
                prompt_line(short_label, buf, ""),
                Line::from(vec![
                    Span::raw(suffix_within_width(buf, width.saturating_sub(1)).to_string()),
                    Span::styled("█", Style::default().fg(Color::Cyan)),
                ]),
            ],
            width,
            |l| l.width(),
        )
        .unwrap_or_default();
        f.render_widget(framed(area, Paragraph::new(line)), area);
        return;
    }
    // The hint, when there is one, is the most useful thing to say, so it
    // takes the place of the last log line.
    let last = match hint {
        Some(text) => Span::styled(text, Style::default().fg(LogLevel::Warn.color())),
        None => state
            .last_global_log
            .as_ref()
            .map(|l| Span::styled(l.message.as_str(), Style::default().fg(l.level.color())))
            .unwrap_or_else(|| Span::raw("")),
    };
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
    let add = if state.accepts_requests {
        " │ n add package"
    } else {
        ""
    };
    let hints = format!(
        "  {nav} │ Tab focus │ l lists{add}{} │ a {} succeeded │ {quit}",
        if retryable {
            " │ r retry (no space left)"
        } else {
            ""
        },
        if state.show_all { "hide" } else { "show" }
    );
    let dim = Style::default().fg(Color::DarkGray);
    // Fullest first: the log line and every key hint, then just the hints,
    // then the focus and quit keys, then quit alone.
    let line = first_fitting(
        vec![
            Line::from(vec![last, Span::styled(hints.clone(), dim)]),
            Line::from(Span::styled(hints, dim)),
            Line::from(Span::styled(format!("  Tab focus │ {quit}"), dim)),
            Line::from(Span::styled(format!("  {quit}"), dim)),
        ],
        text_width(area),
        |l| l.width(),
    )
    .unwrap_or_default();
    f.render_widget(framed(area, Paragraph::new(line)), area);
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
    fn a_waited_on_package_with_a_row_shows_its_status_not_a_second_entry() {
        let mut state = TuiState::new(RunScope::default());
        state.pkg_mut("python-logfire").status = PkgStatus::Running;
        let waiting = state.pkg_mut("python-mcp");
        waiting.status = PkgStatus::MissingDeps;
        waiting.waiting_on = vec!["logfire".to_string(), "typing-inspection".to_string()];
        assert_eq!(
            queued_lines(&state),
            ["python-mcp <- logfire (running), typing-inspection"]
        );
    }

    #[test]
    fn missing_deps_sets_waiting_on_and_a_later_build_clears_it() {
        let mut state = TuiState::new(RunScope::default());
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
    fn pending_counts_only_rows_still_to_build() {
        let mut state = TuiState::new(RunScope::default());
        state.pkg_mut("running");
        state.pkg_mut("built");
        state.pkg_mut("parked");
        state.pkg_mut("failed");
        assert_eq!(state.pending_count(), 4);
        let fin = |pkg: &str, outcome| Event::PackageFinished {
            pkg: pkg.to_string(),
            version: "1".to_string(),
            outcome,
        };
        state.apply(msg("built", fin("built", PackageOutcome::Built)));
        state.apply(msg(
            "parked",
            fin("parked", PackageOutcome::MissingDeps { deps: vec![] }),
        ));
        state.apply(msg(
            "failed",
            fin(
                "failed",
                PackageOutcome::Failed {
                    reason: "x".to_string(),
                },
            ),
        ));
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn queued_and_built_lines_follow_table_order() {
        let mut state = TuiState::new(RunScope::default());
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
    fn run_level_progress_is_kept_for_the_event_log_before_any_package_has_a_row() {
        let mut state = TuiState::new(RunScope::default());
        for message in ["checking a (1/2)", "checking b (2/2)"] {
            state.apply(Msg {
                pkg: None,
                event: Event::Progress {
                    step: None,
                    message: message.to_string(),
                },
            });
        }
        let kept: Vec<&str> = state.run_log.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(kept, ["checking a (1/2)", "checking b (2/2)"]);
        assert!(state.packages.is_empty());
    }

    #[test]
    fn a_finished_step_between_steps_shows_its_label_not_a_dash() {
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut state = TuiState::new(RunScope::default());
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
        let mut browse = BrowseState {
            act_expanded: true,
            filter: Some("jq".to_string()),
            filter_input: Some(String::new()),
            ..Default::default()
        };
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
        let mut browse = BrowseState {
            filter: Some("jq".to_string()),
            filter_input: Some("j".to_string()),
            ..Default::default()
        };
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
        let scope = RunScope {
            project: "home:foo:bar".to_string(),
            targets: vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
        };
        let mut state = TuiState::new(scope);
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
        for (w, h) in [
            (80u16, 30u16),
            (80, 24),
            (60, 15),
            (40, 10),
            (20, 6),
            (20, 8),
            (10, 3),
            (3, 3),
        ] {
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
                                            render(
                                                f,
                                                &state,
                                                &mut table_state,
                                                input,
                                                &browse,
                                                None,
                                            );
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
        let state = TuiState::new(RunScope::default());
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
                render(f, &state, &mut table_state, None, &browse, None);
            })
            .unwrap();
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn render_at(state: &TuiState, w: u16, h: u16) -> Terminal<TestBackend> {
        let browse = BrowseState::default();
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                render(f, state, &mut table_state, None, &browse, None);
            })
            .unwrap();
        terminal
    }

    #[test]
    fn the_header_names_the_project_and_targets_when_there_is_room() {
        let terminal = render_at(&state_with_history(), 120, 30);
        assert!(row_text(&terminal, 1).contains("project: home:foo:bar | target: foo, bar, baz"));
        // Narrower, but still a boxed header: the project survives the cut.
        let terminal = render_at(&state_with_history(), 80, 30);
        assert!(row_text(&terminal, 1).contains("project: home:foo:bar"));
    }

    #[test]
    fn a_terminal_below_the_minimum_gets_one_notice_line() {
        let state = state_with_history();
        let terminal = render_at(&state, 19, 10);
        assert!(row_text(&terminal, 0).starts_with("terminal too small"));
        assert!(!row_text(&terminal, 1).contains("Event Log"));
        // Cut to the width: "termina..." is all that fits in 10 columns.
        let terminal = render_at(&state, 10, 3);
        assert_eq!(row_text(&terminal, 0).trim_end(), "termina...");
    }

    #[test]
    fn the_minimum_size_is_drawn_in_full() {
        let state = state_with_history();
        let terminal = render_at(&state, 20, 6);
        assert!(!row_text(&terminal, 0).contains("terminal too small"));
    }

    #[test]
    fn too_small_is_width_under_20_or_height_under_6() {
        assert!(too_small(Rect::new(0, 0, 19, 30)));
        assert!(too_small(Rect::new(0, 0, 80, 5)));
        assert!(!too_small(Rect::new(0, 0, 20, 6)));
    }

    #[test]
    fn short_terminals_get_bare_summary_and_footer_lines() {
        assert_eq!(stacked_heights(30, 5), [3, 5, 19, 3]);
        assert_eq!(stacked_heights(19, 5), [1, 5, 12, 1]);
        assert_eq!(stacked_heights(6, 5), [1, 2, 2, 1]);
    }

    #[test]
    fn stacked_heights_always_fill_the_terminal() {
        for height in 6..=80u16 {
            for wanted in 5..=12u16 {
                let [summary, table, detail, footer] = stacked_heights(height, wanted);
                assert_eq!(summary + table + detail + footer, height);
                assert!(table <= wanted);
                assert!(table <= detail + table, "table never outgrows the body");
            }
        }
    }

    #[test]
    fn the_table_keeps_the_most_important_columns_when_narrow() {
        assert_eq!(visible_columns(80), vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(visible_columns(60), vec![0, 1, 2, 3, 5]);
        assert_eq!(visible_columns(40), vec![0, 1, 3]);
        // The package name is always there, even when nothing else fits.
        assert_eq!(visible_columns(5), vec![1]);
    }

    #[test]
    fn a_narrow_table_drops_cost_and_keeps_the_package_name() {
        let state = state_with_history();
        let terminal = render_at(&state, 40, 24);
        // Summary takes rows 0-2, so the table header sits on row 4.
        let header = row_text(&terminal, 4);
        assert!(header.contains("Package"));
        assert!(!header.contains("Cost"));
    }

    #[test]
    fn the_run_pane_is_carved_off_only_with_a_log_and_room() {
        let area = Rect::new(0, 0, 80, 20);
        assert_eq!(split_run_pane(area, false), (area, None));
        let small = Rect::new(0, 0, 80, RUN_PANE_MIN_DETAIL - 1);
        assert_eq!(split_run_pane(small, true), (small, None));
        let (detail, run) = split_run_pane(area, true);
        let run = run.expect("a log and room give a pane");
        assert_eq!(detail.height + run.height, area.height);
        assert!((4..=RUN_PANE_MAX_HEIGHT).contains(&run.height));
        assert_eq!(run.y, detail.y + detail.height);
    }

    #[test]
    fn panes_stack_when_narrow_and_sit_side_by_side_when_wide() {
        let wide = two_panes(Rect::new(0, 0, 80, 10), 65);
        assert_eq!(wide[0].y, wide[1].y);
        assert_eq!(wide[0].width + wide[1].width, 80);
        assert_eq!(wide[0].width, 52);

        let narrow = two_panes(Rect::new(0, 0, 40, 10), 65);
        assert_eq!(narrow[0].x, narrow[1].x);
        assert_eq!(narrow[0].width, 40);
        assert_eq!(narrow[0].height + narrow[1].height, 10);
    }

    #[test]
    fn first_fitting_takes_the_first_option_that_fits_else_the_last() {
        let options = vec!["abcdef", "abc", "a"];
        assert_eq!(
            first_fitting(options.clone(), 6, |s| s.len()),
            Some("abcdef")
        );
        assert_eq!(first_fitting(options.clone(), 3, |s| s.len()), Some("abc"));
        assert_eq!(first_fitting(options.clone(), 0, |s| s.len()), Some("a"));
        assert_eq!(first_fitting(Vec::<&str>::new(), 5, |s| s.len()), None);
    }

    #[test]
    fn suffix_keeps_the_end_of_the_text_within_its_width() {
        assert_eq!(suffix_within_width("abcdef", 3), "def");
        assert_eq!(suffix_within_width("abc", 10), "abc");
        assert_eq!(suffix_within_width("abc", 0), "");
        assert_eq!(suffix_within_width("日本", 3), "本");
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// Feeds a recorded run (`AP_REPLAY=<run.jsonl>`) through the table state and
    /// prints what the screen would list. Ignored: it needs a recorded run.
    #[test]
    #[ignore]
    fn replay_a_recorded_run() {
        let Ok(path) = std::env::var("AP_REPLAY") else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let mut state = TuiState::new(RunScope::default());
        let mut skipped = 0;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let ambient = value
                .get("pkg")
                .and_then(|p| p.as_str())
                .map(str::to_string);
            match serde_json::from_value::<Event>(value) {
                Ok(event) => state.apply(Msg {
                    pkg: ambient,
                    event,
                }),
                Err(_) => skipped += 1,
            }
        }
        eprintln!("rows: {} (skipped {skipped} lines)", state.order.len());
        let mut seen = std::collections::HashSet::new();
        for n in &state.order {
            if !seen.insert(n.clone()) {
                eprintln!("DUPLICATE ROW {n}");
            }
        }
        eprintln!("order: {:?}", state.order);
        eprintln!("queued: {:#?}", queued_lines(&state));
        eprintln!("built: {}", built_lines(&state).len());
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    fn scope(targets: &[&str]) -> RunScope {
        RunScope {
            project: "home:foo:bar".to_string(),
            targets: targets.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn counters() -> Vec<String> {
        vec![
            " Auto-Package — 3/10 packages done, 7 pending (2 built) │ elapsed 12s │ cost $0.1234 "
                .to_string(),
            " 3/10 done, 7 pending │ elapsed 12s │ $0.1234 ".to_string(),
            " 3/10 done │ $0.1234 ".to_string(),
        ]
    }

    #[test]
    fn one_target_is_named_with_the_project_alone_as_the_last_resort() {
        assert_eq!(
            scope_labels(&scope(&["pkg1"])),
            [
                "project: home:foo:bar | target: pkg1",
                "project: home:foo:bar"
            ]
        );
    }

    #[test]
    fn targets_that_do_not_fit_are_counted_from_the_end() {
        assert_eq!(
            scope_labels(&scope(&["pkg1", "pkg2", "pkg3"])),
            [
                "project: home:foo:bar | target: pkg1, pkg2, pkg3",
                "project: home:foo:bar | target: pkg1, pkg2 (+1)",
                "project: home:foo:bar | target: pkg1 (+2)",
                "project: home:foo:bar",
            ]
        );
    }

    #[test]
    fn no_targets_means_the_whole_project() {
        assert_eq!(
            scope_labels(&scope(&[])),
            [
                "project: home:foo:bar | target: all packages",
                "project: home:foo:bar",
            ]
        );
    }

    #[test]
    fn a_wide_header_shows_the_full_scope_before_the_full_counters() {
        let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 200);
        assert!(
            line.starts_with("project: home:foo:bar | target: pkg1, pkg2 │ Auto-Package"),
            "{line}"
        );
    }

    #[test]
    fn a_narrow_header_drops_the_scope_before_the_counters_it_cannot_share_room_with() {
        let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 40);
        assert_eq!(line, " 3/10 done │ $0.1234 ");
    }

    #[test]
    fn the_targets_are_dropped_before_the_project() {
        let line = header_line(&scope_labels(&scope(&["pkg1", "pkg2"])), counters(), 60);
        assert_eq!(line, "project: home:foo:bar │ 3/10 done │ $0.1234 ");
    }

    #[test]
    fn a_header_never_outgrows_its_width() {
        let scopes = scope_labels(&scope(&["a-long-package-name", "another-one", "third"]));
        // From 3 columns up: narrower than the "..." cut mark, nothing fits.
        for width in 3..=160 {
            let line = header_line(&scopes, counters(), width);
            assert!(line.width() <= width, "{width}: {line:?}");
        }
    }

    #[test]
    fn a_project_too_long_for_any_width_is_cut_with_an_ellipsis() {
        let long = RunScope {
            project: "home:a-very-long-project-name:branches:openSUSE:Factory".to_string(),
            targets: vec!["pkg".to_string()],
        };
        let line = header_line(&scope_labels(&long), counters(), 20);
        assert!(line.width() <= 20, "{line:?}");
        assert!(line.ends_with("..."), "{line:?}");
    }
}

#[cfg(test)]
mod run_end_tests {
    use super::*;
    use ap_core::event::SelectionReason;

    fn finish(state: &mut TuiState, pkg: &str, outcome: PackageOutcome) {
        state.apply(Msg {
            pkg: None,
            event: Event::PackageFinished {
                pkg: pkg.to_string(),
                version: "1".to_string(),
                outcome,
            },
        });
    }

    fn failed() -> PackageOutcome {
        PackageOutcome::Failed {
            reason: "x".to_string(),
        }
    }

    #[test]
    fn every_row_built_with_no_work_left_is_settled() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "built", PackageOutcome::Built);
        finish(&mut state, "already", PackageOutcome::AlreadyBuilt);
        assert!(run_is_done(&state, 0));
        assert!(run_is_over(&state, 0));
    }

    #[test]
    fn a_failed_row_is_over_but_not_settled() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "built", PackageOutcome::Built);
        finish(&mut state, "failed", failed());
        assert!(!run_is_done(&state, 0));
        assert!(run_is_over(&state, 0));
    }

    #[test]
    fn a_row_still_running_keeps_the_run_open() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "built", PackageOutcome::Built);
        state.pkg_mut("running").status = PkgStatus::Running;
        assert!(!run_is_done(&state, 0));
        assert!(!run_is_over(&state, 0));
    }

    #[test]
    fn an_empty_run_is_neither_settled_nor_over_until_it_has_finished() {
        let state = TuiState::new(RunScope::default());
        assert!(!run_is_done(&state, 0));
        assert!(!run_is_over(&state, 0));
    }

    #[test]
    fn a_retry_asked_for_keeps_the_run_open() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "foo", PackageOutcome::Built);
        state.pkg_mut("foo").retry_requested = true;
        assert!(!run_is_done(&state, 0));
        assert!(!run_is_over(&state, 0));
    }

    #[test]
    fn a_built_row_that_is_re_queued_is_not_settled_until_it_builds_again() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "foo", PackageOutcome::Built);
        assert!(run_is_done(&state, 0));
        state.apply(Msg {
            pkg: None,
            event: Event::TargetSelected {
                pkg: "foo".to_string(),
                version: "1".to_string(),
                reason: SelectionReason::RequestedRoot,
                blocked_by: Vec::new(),
            },
        });
        assert!(!run_is_done(&state, 0));
        assert!(!run_is_over(&state, 0));
        finish(&mut state, "foo", PackageOutcome::Built);
        assert!(run_is_done(&state, 0));
    }

    #[test]
    fn outstanding_work_keeps_the_run_open_even_when_every_row_is_settled() {
        let mut state = TuiState::new(RunScope::default());
        finish(&mut state, "built", PackageOutcome::Built);
        assert!(!run_is_done(&state, 1));
        assert!(!run_is_over(&state, 1));
    }

    #[test]
    fn a_row_parked_on_missing_deps_is_not_settled_even_when_nothing_is_left() {
        let mut state = TuiState::new(RunScope::default());
        let deps = vec!["dep".to_string()];
        finish(&mut state, "parked", PackageOutcome::MissingDeps { deps });
        assert!(!run_is_done(&state, 0));
        assert!(run_is_over(&state, 0));
        assert!(!run_is_done(&state, 1));
    }

    #[test]
    fn a_parked_row_stays_unsettled_once_its_dependency_is_built() {
        let mut state = TuiState::new(RunScope::default());
        let deps = vec!["dep".to_string()];
        finish(&mut state, "parked", PackageOutcome::MissingDeps { deps });
        finish(&mut state, "dep", PackageOutcome::Built);
        assert!(!run_is_done(&state, 0));
        assert!(run_is_over(&state, 0));
    }

    #[test]
    fn the_hint_only_shows_once_the_run_is_over_and_not_settled() {
        assert_eq!(over_hint(false, false, true), None);
        assert_eq!(over_hint(false, true, true), None);
        assert_eq!(over_hint(true, true, true), None);
        assert!(over_hint(true, false, true).is_some());
    }

    #[test]
    fn a_run_that_takes_no_requests_is_told_to_quit_not_to_add() {
        let hint = over_hint(true, false, false).unwrap();
        assert!(hint.contains("q to quit"));
        assert!(!hint.contains("add"));
    }

    #[test]
    fn an_empty_run_that_succeeded_with_no_work_left_is_settled() {
        let mut state = TuiState::new(RunScope::default());
        state.run_outcome = Some(RunOutcome::Success);
        assert!(run_is_done(&state, 0));
    }

    #[test]
    fn an_empty_run_with_work_still_outstanding_is_not_settled() {
        let mut state = TuiState::new(RunScope::default());
        state.run_outcome = Some(RunOutcome::Success);
        assert!(!run_is_done(&state, 1));
    }

    #[test]
    fn an_empty_run_is_not_settled_before_its_outcome_or_after_a_failure() {
        let mut state = TuiState::new(RunScope::default());
        assert!(!run_is_done(&state, 0));
        state.run_outcome = Some(RunOutcome::Failed);
        assert!(!run_is_done(&state, 0));
    }

    #[test]
    fn a_request_is_work_until_sent_and_is_released_if_the_run_is_gone() {
        let work = AtomicUsize::new(0);
        let (add_tx, add_rx) = channel::<AddRequest>();
        assert!(request_build(&add_tx, &work, AddRequest::New("foo".into())));
        assert_eq!(work.load(Ordering::SeqCst), 1);
        assert!(matches!(add_rx.recv().unwrap(), AddRequest::New(n) if n == "foo"));
        drop(add_rx);
        assert!(!request_build(
            &add_tx,
            &work,
            AddRequest::Retry("bar".into())
        ));
        assert_eq!(work.load(Ordering::SeqCst), 1);
    }
}
