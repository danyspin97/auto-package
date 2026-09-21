//! Interactive TUI for real-time build monitoring.
//!
//! Displays build progress, events, agent activity, and token usage in a
//! structured terminal UI using ratatui.

use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};

use ap_core::event::{Event, RunOutcome, StepKind, StepOutcome, Task};
use ap_core::sink::Reporter;

const MAX_LOG_LINES: usize = 100;
const MAX_AGENT_ACTIVITY: usize = 20;

/// State tracking for the TUI display.
#[derive(Clone)]
struct TuiState {
    /// Current package being built
    current_package: Option<(String, String)>, // (name, version)
    /// Current step being executed
    current_step: Option<(u32, StepKind)>,
    /// Recent log messages
    log_lines: Vec<LogLine>,
    /// Agent activity (tool calls, reasoning)
    agent_activity: Vec<String>,
    /// Token usage stats
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cost_usd: f64,
    /// Run outcome
    outcome: Option<RunOutcome>,
    /// Build start time
    start_time: Option<Instant>,
    /// Packages in queue
    packages_in_queue: Vec<String>,
}

#[derive(Clone)]
struct LogLine {
    #[allow(dead_code)]
    timestamp: Instant,
    level: LogLevel,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

impl TuiState {
    fn new() -> Self {
        Self {
            current_package: None,
            current_step: None,
            log_lines: Vec::new(),
            agent_activity: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            outcome: None,
            start_time: None,
            packages_in_queue: Vec::new(),
        }
    }

    fn add_log(&mut self, level: LogLevel, message: String) {
        self.log_lines.push(LogLine {
            timestamp: Instant::now(),
            level,
            message,
        });
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

    fn update_from_event(&mut self, event: &Event) {
        match event {
            Event::RunStarted { .. } => {
                self.start_time = Some(Instant::now());
                self.add_log(LogLevel::Info, "Build run started".to_string());
            }
            Event::RunFinished { outcome, elapsed, usage } => {
                self.outcome = Some(outcome.clone());
                let level = match outcome {
                    RunOutcome::Success => LogLevel::Success,
                    _ => LogLevel::Error,
                };
                self.add_log(
                    level,
                    format!(
                        "Run finished: {} in {:.2}s (${:.4})",
                        outcome,
                        elapsed.as_secs_f64(),
                        usage.cost_usd
                    ),
                );
            }
            Event::TargetSelected { pkg, version, .. } => {
                self.current_package = Some((pkg.clone(), version.clone()));
                self.add_log(
                    LogLevel::Info,
                    format!("Building {} {}", pkg, version),
                );
            }
            Event::StepStarted { step, kind } => {
                self.current_step = Some((step.0, kind.clone()));
                self.add_log(LogLevel::Info, format!("Step {}: {}", step.0 + 1, kind));
            }
            Event::StepFinished { step, outcome, elapsed } => {
                let level = match outcome {
                    StepOutcome::Ok => LogLevel::Success,
                    StepOutcome::Failed => LogLevel::Error,
                    StepOutcome::Skipped => LogLevel::Warn,
                };
                self.add_log(
                    level,
                    format!(
                        "Step {} {}: {} ({:.2}s)",
                        step.0 + 1,
                        outcome,
                        self.current_step
                            .as_ref()
                            .map(|(_, k)| k.to_string())
                            .unwrap_or_default(),
                        elapsed.as_secs_f64()
                    ),
                );
                if matches!(outcome, StepOutcome::Ok | StepOutcome::Skipped) {
                    self.current_step = None;
                }
            }
            Event::BuildFailed { classification, .. } => {
                self.add_log(
                    LogLevel::Error,
                    format!("Build failed: {:?}", classification),
                );
            }
            Event::AgentPrompt { task, .. } => {
                let task_str = match task {
                    Task::Review => "Reviewing spec".to_string(),
                    Task::FixAfterBuild { attempt, max_attempts } => {
                        format!("Fixing spec (attempt {}/{})", attempt, max_attempts)
                    }
                };
                self.add_agent_activity(task_str.clone());
                self.add_log(LogLevel::Info, format!("Agent: {}", task_str));
            }
            Event::AgentReply { response, usage, .. } => {
                self.total_input_tokens += usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
                self.total_cost_usd += usage.cost_usd;

                let activity = format!(
                    "{} (confidence: {}, ${:.4})",
                    response.action,
                    response.confidence.as_deref().unwrap_or("?"),
                    usage.cost_usd
                );
                self.add_agent_activity(activity.clone());

                let reasoning_short = if response.reasoning.len() > 80 {
                    format!("{}...", &response.reasoning[..77])
                } else {
                    response.reasoning.clone()
                };
                self.add_log(
                    LogLevel::Info,
                    format!("Agent {}: {}", response.action, reasoning_short),
                );
            }
            Event::AgentToolUse { tool, input, .. } => {
                let input_short: String = input.chars().take(60).collect();
                let activity = format!("→ {} {}", tool, input_short);
                self.add_agent_activity(activity.clone());
            }
            Event::Progress { message, .. } => {
                self.add_log(LogLevel::Info, message.clone());
            }
            Event::Warning { message, .. } => {
                self.add_log(LogLevel::Warn, message.clone());
            }
            Event::MissingPackages { names, .. } => {
                self.packages_in_queue.extend(names.clone());
                self.add_log(
                    LogLevel::Warn,
                    format!("Missing packages: {}", names.join(", ")),
                );
            }
            Event::SpecChanged { lines_added, lines_removed, .. } => {
                if *lines_added > 0 || *lines_removed > 0 {
                    self.add_log(
                        LogLevel::Success,
                        format!("Spec changed: +{} -{}", lines_added, lines_removed),
                    );
                }
            }
            Event::Command { program, argv, .. } => {
                let cmd = format!("{} {}", program, argv.join(" "));
                if cmd.len() > 100 {
                    self.add_log(LogLevel::Info, format!("$ {}...", &cmd[..97]));
                } else {
                    self.add_log(LogLevel::Info, format!("$ {}", cmd));
                }
            }
            _ => {}
        }
    }
}

/// TUI sink that implements the Reporter trait and displays events in a terminal UI.
pub struct TuiSink {
    sender: Sender<Event>,
}

impl TuiSink {
    /// Create a new TUI sink and spawn the rendering thread.
    pub fn new() -> io::Result<Self> {
        let (sender, receiver) = channel();

        std::thread::spawn(move || {
            if let Err(e) = run_tui(receiver) {
                eprintln!("TUI error: {}", e);
            }
        });

        Ok(Self { sender })
    }
}

impl Reporter for TuiSink {
    fn report(&self, event: &Event) {
        // Non-blocking send - if the TUI thread is gone, just drop the event
        let _ = self.sender.send(event.clone());
    }
}

/// Main TUI rendering loop
fn run_tui(receiver: Receiver<Event>) -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let state = Arc::new(Mutex::new(TuiState::new()));

    // Event processing thread
    let state_clone = Arc::clone(&state);
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            let mut state = state_clone.lock().unwrap();
            state.update_from_event(&event);

            // Exit on RunFinished
            if matches!(event, Event::RunFinished { .. }) {
                break;
            }
        }
    });

    // Rendering loop
    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();

    loop {
        let state = state.lock().unwrap().clone();
        terminal.draw(|f| render_ui(f, &state))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let TermEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        break;
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }

        // Exit when build is finished
        if state.outcome.is_some() {
            // Wait a bit so user can see final state
            std::thread::sleep(Duration::from_secs(2));
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

/// Render the TUI layout
fn render_ui(f: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Length(3),  // Current step
            Constraint::Min(10),    // Event log
            Constraint::Length(12), // Agent activity
            Constraint::Length(3),  // Stats footer
        ])
        .split(f.area());

    render_header(f, state, chunks[0]);
    render_current_step(f, state, chunks[1]);
    render_event_log(f, state, chunks[2]);
    render_agent_activity(f, state, chunks[3]);
    render_stats(f, state, chunks[4]);
}

fn render_header(f: &mut Frame, state: &TuiState, area: Rect) {
    let title = if let Some((name, version)) = &state.current_package {
        format!("Building: {} {}", name, version)
    } else {
        "Auto-Package Build Monitor".to_string()
    };

    let header = Paragraph::new(title)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(header, area);
}

fn render_current_step(f: &mut Frame, state: &TuiState, area: Rect) {
    let text = if let Some((step_num, kind)) = &state.current_step {
        format!("Step {}: {}", step_num + 1, kind)
    } else {
        "Waiting...".to_string()
    };

    let step_widget = Paragraph::new(text)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("Current Step"));

    f.render_widget(step_widget, area);
}

fn render_event_log(f: &mut Frame, state: &TuiState, area: Rect) {
    let items: Vec<ListItem> = state
        .log_lines
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .rev()
        .map(|log| {
            let style = match log.level {
                LogLevel::Info => Style::default().fg(Color::White),
                LogLevel::Warn => Style::default().fg(Color::Yellow),
                LogLevel::Error => Style::default().fg(Color::Red),
                LogLevel::Success => Style::default().fg(Color::Green),
            };

            ListItem::new(log.message.clone()).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Event Log"));

    f.render_widget(list, area);
}

fn render_agent_activity(f: &mut Frame, state: &TuiState, area: Rect) {
    let items: Vec<ListItem> = state
        .agent_activity
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .rev()
        .map(|activity| {
            ListItem::new(activity.clone())
                .style(Style::default().fg(Color::Magenta))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Agent Activity"));

    f.render_widget(list, area);
}

fn render_stats(f: &mut Frame, state: &TuiState, area: Rect) {
    let elapsed = state
        .start_time
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0);

    let stats = vec![
        format!(
            "Tokens: {} in / {} out",
            state.total_input_tokens, state.total_output_tokens
        ),
        format!("Cost: ${:.4}", state.total_cost_usd),
        format!("Elapsed: {}s", elapsed),
    ];

    let text = stats.join(" │ ");
    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL).title("Stats"))
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}
