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
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode},
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
use ap_core::sink::Reporter;

const MAX_LOG_LINES: usize = 300;
const MAX_AGENT_ACTIVITY: usize = 100;

/// Handle used by the CLI to obtain reporters that feed the TUI.
///
/// Cheap to clone; every clone shares the same channel to the render thread.
#[derive(Clone)]
pub struct TuiController {
    tx: Sender<Msg>,
}

struct Msg {
    pkg: Option<String>,
    event: Event,
}

impl TuiController {
    /// Spawn the render thread and return a handle to feed it events.
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if let Err(e) = run(rx) {
                eprintln!("TUI error: {e}");
            }
        });
        Ok(Self { tx })
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
        std::sync::Arc::new(Tapped { base, pkg, tx: self.tx.clone() })
    }
}

struct Tapped {
    base: std::sync::Arc<dyn Reporter + Send + Sync>,
    pkg: Option<String>,
    tx: Sender<Msg>,
}

impl Reporter for Tapped {
    fn report(&self, event: &Event) {
        self.base.report(event);
        let _ = self.tx.send(Msg { pkg: self.pkg.clone(), event: event.clone() });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

impl LogLevel {
    fn color(self) -> Color {
        match self {
            LogLevel::Info => Color::White,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Error => Color::Red,
            LogLevel::Success => Color::Green,
        }
    }
}

#[derive(Clone)]
struct LogLine {
    level: LogLevel,
    message: String,
}

#[derive(Clone, PartialEq, Eq)]
enum PkgStatus {
    Queued,
    Running,
    Succeeded,
    AlreadyBuilt,
    MissingDeps,
    Failed,
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
        }
    }

    fn color(&self) -> Color {
        match self {
            PkgStatus::Queued => Color::DarkGray,
            PkgStatus::Running => Color::Yellow,
            PkgStatus::Succeeded | PkgStatus::AlreadyBuilt => Color::Green,
            PkgStatus::MissingDeps => Color::Magenta,
            PkgStatus::Failed => Color::Red,
        }
    }
}

struct PkgState {
    version: String,
    status: PkgStatus,
    current_step: Option<(u32, StepKind)>,
    log_lines: Vec<LogLine>,
    agent_activity: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    started: Instant,
    finished: bool,
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
        }
    }

    fn add_log(&mut self, level: LogLevel, message: String) {
        self.log_lines.push(LogLine { level, message });
        if self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.remove(0);
        }
    }

    fn add_agent_activity(&mut self, activity: String) {
        self.agent_activity.push(activity);
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
}

impl TuiState {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            packages: HashMap::new(),
            last_global_log: None,
            run_outcome: None,
            run_start: None,
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
        self.last_global_log = Some(LogLine { level, message });
    }

    fn pkg_mut(&mut self, name: &str) -> &mut PkgState {
        if !self.packages.contains_key(name) {
            self.order.push(name.to_string());
            self.packages.insert(name.to_string(), PkgState::new(String::new()));
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
            Event::RunFinished { outcome, elapsed, usage } => {
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
            Event::ResolveFinished { total, built, remaining } => {
                self.add_global_log(
                    LogLevel::Info,
                    format!("Resolved: {total} total, {built} built, {remaining} remaining"),
                );
            }
            Event::TargetSelected { pkg, version, .. } => {
                let state = self.pkg_mut(pkg);
                state.version = version.clone();
                state.status = PkgStatus::Queued;
                state.started = Instant::now();
            }
            Event::PackageFinished { pkg, version, outcome } => {
                let state = self.pkg_mut(pkg);
                if state.version.is_empty() {
                    state.version = version.clone();
                }
                state.finished = true;
                state.status = match outcome {
                    PackageOutcome::Built => PkgStatus::Succeeded,
                    PackageOutcome::AlreadyBuilt => PkgStatus::AlreadyBuilt,
                    PackageOutcome::MissingDeps { .. } => PkgStatus::MissingDeps,
                    PackageOutcome::Failed { .. } => PkgStatus::Failed,
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
            Event::StepStarted { step, kind } => {
                state.status = PkgStatus::Running;
                state.current_step = Some((step.0, kind.clone()));
                state.add_log(LogLevel::Info, format!("Step {}: {kind}", step.0 + 1));
            }
            Event::StepFinished { step, outcome, elapsed } => {
                let kind = state
                    .current_step
                    .as_ref()
                    .map(|(_, k)| k.to_string())
                    .unwrap_or_default();
                let level = match outcome {
                    StepOutcome::Ok => LogLevel::Success,
                    StepOutcome::Failed => LogLevel::Error,
                    StepOutcome::Skipped => LogLevel::Warn,
                };
                state.add_log(
                    level,
                    format!(
                        "Step {} {outcome}: {kind} ({:.2}s)",
                        step.0 + 1,
                        elapsed.as_secs_f64()
                    ),
                );
                if matches!(outcome, StepOutcome::Ok | StepOutcome::Skipped) {
                    state.current_step = None;
                }
            }
            Event::BuildFailed { classification, .. } => {
                state.add_log(LogLevel::Error, format!("Build failed: {classification:?}"));
            }
            Event::AgentPrompt { task, .. } => {
                let task_str = match task {
                    Task::Review => "Reviewing spec".to_string(),
                    Task::FixAfterBuild { attempt, max_attempts } => {
                        format!("Fixing spec (attempt {attempt}/{max_attempts})")
                    }
                };
                state.add_agent_activity(task_str.clone());
                state.add_log(LogLevel::Info, format!("Agent: {task_str}"));
            }
            Event::AgentReply { response, usage, .. } => {
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd += usage.cost_usd;
                state.add_agent_activity(format!(
                    "{} (confidence: {}, ${:.4})",
                    response.action,
                    response.confidence.as_deref().unwrap_or("?"),
                    usage.cost_usd
                ));
                let reasoning = if response.reasoning.chars().count() > 80 {
                    format!("{}...", response.reasoning.chars().take(77).collect::<String>())
                } else {
                    response.reasoning.clone()
                };
                state.add_log(LogLevel::Info, format!("Agent {}: {reasoning}", response.action));
            }
            Event::AgentToolUse { tool, input, .. } => {
                let input_short: String = input.chars().take(60).collect();
                state.add_agent_activity(format!("→ {tool} {input_short}"));
            }
            Event::Progress { message, .. } => {
                state.add_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } => {
                state.add_log(LogLevel::Warn, message.clone());
            }
            Event::MissingPackages { names, .. } => {
                state.add_log(LogLevel::Warn, format!("Missing packages: {}", names.join(", ")));
            }
            Event::SpecChanged { lines_added, lines_removed, .. } => {
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

fn run(rx: Receiver<Msg>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // However the event loop ends — clean exit, a render/input error, or a
    // panic unwinding through here — raw mode and the alternate screen must
    // be torn down. Left engaged, every line printed afterwards (including
    // the error itself) lands inside the broken terminal state instead of
    // the user's normal screen.
    let result = event_loop(&mut terminal, rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: Receiver<Msg>,
) -> io::Result<()> {
    let mut state = TuiState::new();
    let mut table_state = TableState::default();
    let tick_rate = Duration::from_millis(150);
    let mut finished_at: Option<Instant> = None;

    loop {
        // Drain everything currently queued so a burst of events (e.g. many
        // packages starting at once) renders as one frame, not one per event.
        loop {
            match rx.try_recv() {
                Ok(msg) => state.apply(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished_at.get_or_insert(Instant::now());
                    break;
                }
            }
        }

        if table_state.selected().is_none() && !state.order.is_empty() {
            table_state.select(Some(0));
        }

        terminal.draw(|f| render(f, &state, &mut table_state))?;

        if event::poll(tick_rate)?
            && let TermEvent::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    break;
                }
                KeyCode::Down | KeyCode::Char('j') => select_next(&mut table_state, &state),
                KeyCode::Up | KeyCode::Char('k') => select_prev(&mut table_state, &state),
                _ => {}
            }
        }

        if state.run_outcome.is_some() {
            let at = finished_at.get_or_insert(Instant::now());
            if at.elapsed() >= Duration::from_secs(2) {
                break;
            }
        }
    }

    Ok(())
}

fn select_next(table_state: &mut TableState, state: &TuiState) {
    if state.order.is_empty() {
        return;
    }
    let i = table_state.selected().unwrap_or(0);
    table_state.select(Some((i + 1).min(state.order.len() - 1)));
}

fn select_prev(table_state: &mut TableState, _state: &TuiState) {
    let i = table_state.selected().unwrap_or(0);
    table_state.select(Some(i.saturating_sub(1)));
}

fn render(f: &mut Frame, state: &TuiState, table_state: &mut TableState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                     // Summary header
            Constraint::Length((state.order.len() as u16 + 3).clamp(5, 12)), // Package table
            Constraint::Min(8),                         // Detail pane
            Constraint::Length(3),                      // Global log / footer
        ])
        .split(f.area());

    render_summary(f, state, chunks[0]);
    render_table(f, state, chunks[1], table_state);
    render_detail(f, state, chunks[2], table_state.selected());
    render_footer(f, state, chunks[3]);
}

fn render_summary(f: &mut Frame, state: &TuiState, area: Rect) {
    let done = state.packages.values().filter(|p| p.finished).count();
    let total = state.order.len();
    let elapsed = state.run_start.map(|s| s.elapsed().as_secs()).unwrap_or(0);
    let title = format!(
        " Auto-Package — {done}/{total} packages done │ elapsed {elapsed}s │ cost ${:.4} ",
        state.total_cost_usd()
    );
    let header = Paragraph::new(title)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, area);
}

fn render_table(f: &mut Frame, state: &TuiState, area: Rect, table_state: &mut TableState) {
    let rows: Vec<Row> = state
        .order
        .iter()
        .map(|name| {
            let pkg = &state.packages[name];
            let step = pkg
                .current_step
                .as_ref()
                .map(|(n, k)| format!("{}: {k}", n + 1))
                .unwrap_or_else(|| "-".to_string());
            let elapsed = pkg.started.elapsed().as_secs();
            Row::new(vec![
                Cell::from(pkg.status.label()).style(Style::default().fg(pkg.status.color())),
                Cell::from(name.as_str()),
                Cell::from(pkg.version.as_str()),
                Cell::from(step),
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
        Constraint::Length(9),
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["Status", "Package", "Version", "Step", "Time", "Cost"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(Block::default().borders(Borders::ALL).title("Packages"));

    f.render_stateful_widget(table, area, table_state);
}

fn render_detail(f: &mut Frame, state: &TuiState, area: Rect, selected: Option<usize>) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    let Some(pkg) = selected.and_then(|i| state.order.get(i)).and_then(|n| state.packages.get(n))
    else {
        f.render_widget(
            Paragraph::new("No package selected yet").block(
                Block::default().borders(Borders::ALL).title("Event Log"),
            ),
            chunks[0],
        );
        f.render_widget(
            Block::default().borders(Borders::ALL).title("Agent Activity"),
            chunks[1],
        );
        return;
    };

    let log_items: Vec<ListItem> = pkg
        .log_lines
        .iter()
        .rev()
        .take(chunks[0].height.saturating_sub(2) as usize)
        .rev()
        .map(|l| ListItem::new(l.message.clone()).style(Style::default().fg(l.level.color())))
        .collect();
    let log = List::new(log_items)
        .block(Block::default().borders(Borders::ALL).title("Event Log"));
    f.render_widget(log, chunks[0]);

    let activity_items: Vec<ListItem> = pkg
        .agent_activity
        .iter()
        .rev()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .rev()
        .map(|a| ListItem::new(a.clone()).style(Style::default().fg(Color::Magenta)))
        .collect();
    let activity = List::new(activity_items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Agent Activity — {} in / {} out",
            pkg.input_tokens, pkg.output_tokens
        )));
    f.render_widget(activity, chunks[1]);
}

fn render_footer(f: &mut Frame, state: &TuiState, area: Rect) {
    let last = state
        .last_global_log
        .as_ref()
        .map(|l| Span::styled(l.message.clone(), Style::default().fg(l.level.color())))
        .unwrap_or_else(|| Span::raw(""));
    let hint = Span::styled(
        "  ↑/↓ select package │ q/Esc quit",
        Style::default().fg(Color::DarkGray),
    );
    let line = Line::from(vec![last, hint]);
    let footer = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    f.render_widget(footer, area);
}
