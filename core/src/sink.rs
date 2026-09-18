use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use owo_colors::{OwoColorize, Stream::Stderr, Style};

use crate::event::{Event, RunOutcome, StepOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl std::str::FromStr for LogLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(format!("unknown log level: {s}")),
        }
    }
}

pub trait Reporter: Send + Sync {
    fn report(&self, event: &Event);
}

impl<T: Reporter + ?Sized> Reporter for std::sync::Arc<T> {
    fn report(&self, event: &Event) {
        (**self).report(event);
    }
}

/// Discards every event. Used for subprocesses whose individual invocations
/// are not worth recording — e.g. the 15-second poll inside a build wait.
pub struct NullReporter;

impl Reporter for NullReporter {
    fn report(&self, _event: &Event) {}
}

pub static NULL_REPORTER: NullReporter = NullReporter;

pub struct FanoutReporter {
    sinks: Vec<Box<dyn Reporter>>,
}

impl FanoutReporter {
    pub fn new(sinks: Vec<Box<dyn Reporter>>) -> Self {
        Self { sinks }
    }
}

impl Reporter for FanoutReporter {
    fn report(&self, event: &Event) {
        for s in &self.sinks {
            s.report(event);
        }
    }
}

pub struct HumanSink {
    level: LogLevel,
}

impl HumanSink {
    pub fn new(level: LogLevel) -> Self {
        Self { level }
    }

    fn event_level(event: &Event) -> LogLevel {
        match event {
            Event::RunFinished { outcome, .. } => match outcome {
                RunOutcome::Success => LogLevel::Info,
                _ => LogLevel::Error,
            },
            Event::BuildFailed { .. } => LogLevel::Error,
            Event::Warning { .. } => LogLevel::Warn,
            Event::MissingPackages { .. } => LogLevel::Warn,
            Event::SpecRejected { .. } => LogLevel::Warn,
            Event::RunStarted { .. }
            | Event::ResolveStarted { .. }
            | Event::ResolveFinished { .. }
            | Event::TargetSelected { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::SpecChanged { .. }
            | Event::Progress { .. } => LogLevel::Info,
            Event::Command { .. }
            | Event::CommandDone { .. }
            | Event::ExtractResult { .. }
            | Event::AgentPrompt { .. }
            | Event::AgentReply { .. }
            | Event::AgentToolUse { .. }
            | Event::Artifact { .. } => LogLevel::Debug,
        }
    }

    fn format(&self, event: &Event) -> Option<String> {
        match event {
            Event::RunStarted { run_id, work_dir, .. } => Some(format!(
                "{} · run {} · {}",
                "auto-package".if_supports_color(Stderr, |t| t.bold()),
                run_id.if_supports_color(Stderr, |t| t.dimmed()),
                work_dir.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::RunFinished { outcome, elapsed, usage } => {
                let ms = elapsed.as_millis();
                let (secs, frac) = (ms / 1000, ms % 1000);
                let outcome_colored = match outcome {
                    RunOutcome::Success => format!(
                        "{}",
                        "Success"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    _ => format!(
                        "{}",
                        "Failed"
                            .if_supports_color(Stderr, |t| t.style(Style::new().red().bold()))
                    ),
                };
                Some(format!(
                    "run finished: {outcome_colored} in {secs}.{frac:03}s · {}",
                    format!("${:.4}", usage.cost_usd)
                        .if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::ResolveStarted { root } => Some(format!(
                "resolve {}",
                root.if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::ResolveFinished { total, built, remaining } => Some(format!(
                "  {} packages in closure · {} already built · {} remaining",
                total,
                built.if_supports_color(Stderr, |t| t.green()),
                remaining,
            )),
            Event::TargetSelected { pkg, version, blocked_by, .. } => {
                let mut s = format!(
                    "building {} {}",
                    pkg.if_supports_color(Stderr, |t| t.bold()),
                    version.if_supports_color(Stderr, |t| t.dimmed()),
                );
                if !blocked_by.is_empty() {
                    s.push_str(&format!(
                        "  {}",
                        format!("(blocked by: {})", blocked_by.join(", "))
                            .if_supports_color(Stderr, |t| t.yellow()),
                    ));
                }
                Some(s)
            }
            Event::StepStarted { step, kind } => Some(format!(
                "  {} {}",
                format!("{:02}", step.0 + 1).if_supports_color(Stderr, |t| t.dimmed()),
                kind.if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::StepFinished { step, outcome, elapsed } => {
                let ms = elapsed.as_millis();
                let (secs, frac) = (ms / 1000, ms % 1000);
                let outcome_colored = match outcome {
                    StepOutcome::Ok => {
                        format!("{}", "ok".if_supports_color(Stderr, |t| t.green()))
                    }
                    StepOutcome::Failed => {
                        format!("{}", "failed".if_supports_color(Stderr, |t| t.red()))
                    }
                    StepOutcome::Skipped => {
                        format!("{}", "skipped".if_supports_color(Stderr, |t| t.yellow()))
                    }
                };
                Some(format!(
                    "  {} done  {}  {outcome_colored}",
                    format!("{:02}", step.0 + 1).if_supports_color(Stderr, |t| t.dimmed()),
                    format!("{secs}.{frac:03}s").if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::BuildFailed { classification, log, .. } => {
                let label = match classification {
                    crate::event::BuildFailure::RpmParseError { message, .. } => {
                        format!("rpm parse error: {message}")
                    }
                    crate::event::BuildFailure::PrepFailed => "prep failed".to_string(),
                    crate::event::BuildFailure::BuildFailed => "build failed".to_string(),
                    crate::event::BuildFailure::InstallFailed => "install failed".to_string(),
                    crate::event::BuildFailure::CheckFailed => "check failed".to_string(),
                    crate::event::BuildFailure::UnresolvableDeps { packages } => {
                        format!("unresolvable deps: {}", packages.join(", "))
                    }
                    crate::event::BuildFailure::Unclassified { raw } => {
                        format!("failed: {raw}")
                    }
                };
                Some(format!(
                    "  {}: {label}\n  log: {}",
                    "FAILED"
                        .if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
                    log.display().if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::Progress { message, .. } => Some(format!(
                "     {}",
                message.if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::Warning { message, .. } => Some(format!(
                "  {}: {message}",
                "warning".if_supports_color(Stderr, |t| t.style(Style::new().yellow().bold())),
            )),
            Event::MissingPackages { names, .. } => Some(format!(
                "  {}: {}",
                "missing packages".if_supports_color(Stderr, |t| t.yellow()),
                names.join(", "),
            )),
            Event::SpecChanged { lines_added, lines_removed, diff, artifact, .. } => {
                if *lines_added == 0 && *lines_removed == 0 {
                    return None;
                }
                let mut s = format!(
                    "  spec: {} {}",
                    format!("+{lines_added}").if_supports_color(Stderr, |t| t.green()),
                    format!("-{lines_removed}").if_supports_color(Stderr, |t| t.red()),
                );
                if self.level >= LogLevel::Debug {
                    s.push('\n');
                    s.push_str(&delimited("spec diff", diff, artifact));
                }
                Some(s)
            }
            Event::Command { program, argv, cwd, log, .. } => {
                let mut s = format!(
                    "  {} {} {} {}",
                    "+".if_supports_color(Stderr, |t| t.dimmed()),
                    program.if_supports_color(Stderr, |t| t.bold()),
                    argv.join(" ").if_supports_color(Stderr, |t| t.dimmed()),
                    format!("(in {})", cwd.display()).if_supports_color(Stderr, |t| t.dimmed()),
                );
                if let Some(log_path) = log {
                    s.push_str(&format!(
                        "\n    log: {}",
                        log_path.display().if_supports_color(Stderr, |t| t.dimmed()),
                    ));
                }
                Some(s)
            }
            Event::AgentPrompt { task, artifact, .. } => Some(format!(
                "  {}: {task} → {}",
                "agent".if_supports_color(Stderr, |t| t.style(Style::new().cyan().bold())),
                artifact.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::ExtractResult { extract, artifact, .. } => {
                let mut body = format!(
                    "genre: {}\nphase: {}\nlocated by: {}\nelided: {} line(s)\n",
                    extract.genre.as_deref().unwrap_or("—"),
                    extract.phase.as_deref().unwrap_or("—"),
                    extract.located_by.as_deref().unwrap_or("—"),
                    extract.elided_count,
                );
                if !extract.has_signal {
                    body.push_str("extractor found no signal — showing the log tail\n");
                }
                if !extract.source_candidates.is_empty() {
                    body.push_str("source candidates:\n");
                    for c in &extract.source_candidates {
                        body.push_str(&format!("  {}\n", c.display()));
                    }
                }
                if !extract.error_lines.is_empty() {
                    body.push_str("error lines:\n");
                    for l in &extract.error_lines {
                        body.push_str(&format!("  {l}\n"));
                    }
                }
                body.push_str("excerpt:\n");
                body.push_str(&extract.excerpt);
                Some(delimited("extract", &body, artifact))
            }
            Event::AgentToolUse { tool, input, .. } => {
                let summary: String = input.chars().take(160).collect();
                Some(format!(
                    "    {} {tool} {}",
                    "→".if_supports_color(Stderr, |t| t.dimmed()),
                    summary.if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::Artifact { name, path, .. } => Some(format!(
                "    {name}: {}",
                path.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::AgentReply { response, usage, raw, artifact, .. } => {
                let confidence = response.confidence.as_deref().unwrap_or("?");
                let confidence_colored = match confidence {
                    "High" => format!("{}", confidence.if_supports_color(Stderr, |t| t.green())),
                    "Medium" => format!("{}", confidence.if_supports_color(Stderr, |t| t.yellow())),
                    _ => format!("{}", confidence.if_supports_color(Stderr, |t| t.red())),
                };
                let action_colored = match response.action.as_str() {
                    "Edited" => format!(
                        "{}",
                        response.action.if_supports_color(Stderr, |t| t.green())
                    ),
                    "NoChange" => format!(
                        "{}",
                        response.action.if_supports_color(Stderr, |t| t.yellow())
                    ),
                    _ => format!(
                        "{}",
                        response.action.if_supports_color(Stderr, |t| t.red())
                    ),
                };
                let reasoning = if response.reasoning.chars().count() > 120 {
                    format!(
                        "{}…",
                        response.reasoning.chars().take(120).collect::<String>()
                    )
                } else {
                    response.reasoning.clone()
                };
                let mut s = format!(
                    "  {} ({action_colored} · {confidence_colored} · {}): {reasoning}",
                    "reply".if_supports_color(Stderr, |t| t.cyan()),
                    format!("${:.4}", usage.cost_usd)
                        .if_supports_color(Stderr, |t| t.dimmed()),
                );
                if self.level >= LogLevel::Trace && !raw.is_empty() {
                    s.push('\n');
                    s.push_str(&delimited("agent response", raw, artifact));
                }
                Some(s)
            }
            _ => None,
        }
    }
}

/// A grep-able dump: a titled rule, the body, and a footer naming the file it
/// was also written to. REDESIGN §2.2 — every `--log debug` dump is delimited.
fn delimited(title: &str, body: &str, artifact: &Path) -> String {
    let rule = "─".repeat(58usize.saturating_sub(title.len()));
    format!(
        "── {title} ── {rule}\n{}\n── end {title} ── also at {}",
        body.trim_end(),
        artifact.display(),
    )
}

impl Reporter for HumanSink {
    fn report(&self, event: &Event) {
        let level = Self::event_level(event);
        if level > self.level {
            return;
        }
        if let Some(msg) = self.format(event) {
            eprintln!("{msg}");
        }
    }
}

pub struct JsonlSink {
    writer: Mutex<BufWriter<File>>,
}

impl JsonlSink {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
        })
    }
}

impl Reporter for JsonlSink {
    fn report(&self, event: &Event) {
        if let Ok(line) = serde_json::to_string(event) {
            if let Ok(mut w) = self.writer.lock() {
                let _ = writeln!(w, "{line}");
                let _ = w.flush();
            }
        }
    }
}

pub struct MemorySink {
    events: Mutex<Vec<Event>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for MemorySink {
    fn report(&self, event: &Event) {
        self.events.lock().unwrap().push(event.clone());
    }
}
