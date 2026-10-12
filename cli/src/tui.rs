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
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState, Wrap},
};

use ap_core::event::{Event, PackageOutcome, RunOutcome, StepKind, StepOutcome, Task};
use ap_core::sink::{MemorySink, Reporter};

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
            PkgStatus::Running => Color::Yellow,
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
    text.to_ascii_lowercase().contains("no space left on device")
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

struct PkgState {
    version: String,
    status: PkgStatus,
    current_step: Option<StepKind>,
    log_lines: Vec<LogLine>,
    agent_activity: Vec<ActivityEntry>,
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
}

impl PkgState {
    fn new(version: String) -> Self {
        Self {
            version,
            status: PkgStatus::Queued,
            current_step: None,
            log_lines: Vec::new(),
            agent_activity: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            started: Instant::now(),
            finished: false,
            tries: 0,
            max_tries: 0,
            disk_full_detected: false,
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
        if self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.remove(0);
        }
    }

    fn add_agent_activity(&mut self, kind: ActivityKind, text: String) {
        self.agent_activity.push(ActivityEntry { kind, text });
        if self.agent_activity.len() > MAX_AGENT_ACTIVITY {
            self.agent_activity.remove(0);
        }
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
                // A fresh attempt — the condition that caused last time's
                // `NoSpace` may already be gone (that's the whole point of
                // the `r` retry keybinding), so don't carry the flag over
                // and have it immediately reclassify a *new* failure.
                state.disk_full_detected = false;
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
            Event::StepStarted { kind, .. } => {
                state.status = PkgStatus::Running;
                state.current_step = Some(kind.clone());
                state.add_step_header(format!("▸ {}", kind.label()));
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
                if matches!(outcome, StepOutcome::Ok | StepOutcome::Skipped) {
                    state.current_step = None;
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
            Event::AgentReply {
                response, usage, ..
            } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
                state.add_agent_activity(
                    ActivityKind::Reply,
                    format!(
                        "{} (confidence: {}, ${:.4})",
                        response.action,
                        response.confidence.as_deref().unwrap_or("?"),
                        usage.cost_usd
                    ),
                );
                let reasoning = if response.reasoning.chars().count() > 80 {
                    format!(
                        "{}...",
                        response.reasoning.chars().take(77).collect::<String>()
                    )
                } else {
                    response.reasoning.clone()
                };
                state.add_log(
                    LogLevel::Info,
                    format!("Agent {}: {reasoning}", response.action),
                );
            }
            Event::AgentToolUse { tool, input, .. } => {
                let input_short: String = input.chars().take(60).collect();
                state.add_agent_activity(ActivityKind::ToolUse, format!("→ {tool} {input_short}"));
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
                if cmd.chars().count() > 100 {
                    state.add_log(
                        LogLevel::Info,
                        format!("$ {}...", cmd.chars().take(97).collect::<String>()),
                    );
                } else {
                    state.add_log(LogLevel::Info, format!("$ {cmd}"));
                }
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
    // Lines scrolled up from the live tail in the selected package's Event
    // Log; 0 means pinned to the latest line. Reset whenever the selection
    // changes, since a scroll offset for one package's log is meaningless
    // for another's.
    let mut log_scroll: usize = 0;
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
            let previous = selected.clone();
            if selected
                .as_deref()
                .is_none_or(|s| !visible.iter().any(|n| n.as_str() == s))
            {
                selected = visible.first().map(|n| (*n).clone());
            }
            if selected != previous {
                log_scroll = 0;
            }
            table_state.select(
                selected
                    .as_ref()
                    .and_then(|s| visible.iter().position(|n| n.as_str() == s)),
            );
        }

        terminal.draw(|f| render(f, &state, &mut table_state, input.as_deref(), log_scroll))?;

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
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('+') => input = Some(String::new()),
                // Quit means quit: end the whole process (killing the running
                // builds), not just the UI. Detaching used to leave the
                // pipeline running headless with nothing on screen and no
                // way to stop it short of `kill`.
                KeyCode::Char('q') | KeyCode::Esc => crate::handle_interrupt(true, memory),
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    // Raw mode disables signal generation entirely, so the OS
                    // never delivers a real SIGINT while the TUI is running —
                    // this keypress IS the only place Ctrl-C shows up. Same
                    // shutdown as `q`/Esc above.
                    crate::handle_interrupt(true, memory);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    select_next(&mut selected, &state);
                    log_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    select_prev(&mut selected, &state);
                    log_scroll = 0;
                }
                KeyCode::Char('a') => state.show_all = !state.show_all,
                // Retry a package that failed with "No space left on
                // device" (`PkgStatus::NoSpace`) once the user has freed
                // some — re-sent through the same channel as the `n` add
                // prompt, since from the build loop's side a retry and a
                // freshly typed package name are the same request. Any
                // other status is a no-op: there is nothing a disk cleanup
                // would fix for a real spec defect.
                KeyCode::Char('r') => {
                    if let Some(name) = selected.as_ref()
                        && state.packages.get(name.as_str()).map(|p| &p.status)
                            == Some(&PkgStatus::NoSpace)
                    {
                        if add_tx.send(name.clone()).is_ok() {
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
                KeyCode::PageUp => log_scroll = log_scroll.saturating_add(10),
                KeyCode::PageDown => log_scroll = log_scroll.saturating_sub(10),
                KeyCode::End => log_scroll = 0,
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

fn render(
    f: &mut Frame,
    state: &TuiState,
    table_state: &mut TableState,
    input: Option<&str>,
    log_scroll: usize,
) {
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
    render_table(f, state, &visible, chunks[1], table_state);
    render_detail(
        f,
        state,
        &visible,
        chunks[2],
        table_state.selected(),
        log_scroll,
    );
    let retryable = table_state
        .selected()
        .and_then(|i| visible.get(i))
        .and_then(|name| state.packages.get(name.as_str()))
        .is_some_and(|p| p.status == PkgStatus::NoSpace);
    render_footer(f, state, input, chunks[3], retryable);
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
) {
    let rows: Vec<Row> = visible
        .iter()
        .map(|name| {
            let pkg = &state.packages[*name];
            let step = pkg
                .current_step
                .as_ref()
                .map(|k| k.label().to_string())
                .unwrap_or_else(|| "-".to_string());
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
        .block(Block::default().borders(Borders::ALL).title("Packages"));

    f.render_stateful_widget(table, area, table_state);
}

fn render_detail(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    selected: Option<usize>,
    log_scroll: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    let Some(pkg) = selected
        .and_then(|i| visible.get(i))
        .and_then(|n| state.packages.get(n.as_str()))
    else {
        f.render_widget(
            Paragraph::new("No package selected yet")
                .block(Block::default().borders(Borders::ALL).title("Event Log")),
            chunks[0],
        );
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Agent Activity"),
            chunks[1],
        );
        return;
    };

    let viewport = chunks[0].height.saturating_sub(2) as usize;
    let total = pkg.log_lines.len();
    let max_scroll = total.saturating_sub(viewport);
    let scroll = log_scroll.min(max_scroll);
    let end = total - scroll;
    let start = end.saturating_sub(viewport);
    let log_items: Vec<ListItem> = pkg.log_lines[start..end]
        .iter()
        .map(|l| {
            let mut style = Style::default().fg(l.level.color());
            if l.emphasis {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(l.message.clone()).style(style)
        })
        .collect();
    let log_title = if scroll > 0 {
        format!("Event Log — scrolled up {scroll} line(s), End to jump to latest")
    } else {
        "Event Log".to_string()
    };
    let log = List::new(log_items).block(Block::default().borders(Borders::ALL).title(log_title));
    f.render_widget(log, chunks[0]);

    let activity_items: Vec<ListItem> = pkg
        .agent_activity
        .iter()
        .rev()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .rev()
        .map(|a| ListItem::new(a.text.clone()).style(Style::default().fg(a.kind.color())))
        .collect();
    let activity =
        List::new(activity_items).block(Block::default().borders(Borders::ALL).title(format!(
            "Agent Activity — {} in / {} out",
            pkg.input_tokens, pkg.output_tokens
        )));
    f.render_widget(activity, chunks[1]);
}

fn render_footer(f: &mut Frame, state: &TuiState, input: Option<&str>, area: Rect, retryable: bool) {
    if let Some(buf) = input {
        let line = Line::from(vec![
            Span::styled(
                "add package (name or repo URL): ",
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(buf.to_string()),
            Span::styled("█", Style::default().fg(Color::Cyan)),
            Span::styled(
                "   Enter queue │ Esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
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
    let hint = Span::styled(
        format!(
            "  ↑/↓ select │ PgUp/PgDn/End scroll log │ n add package{} │ a {} succeeded │ q/Esc quit",
            if retryable { " │ r retry (no space left)" } else { "" },
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
