use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use owo_colors::{OwoColorize, Stream::Stderr, Style};
use serde::Serialize;

use crate::event::{Event, PackageOutcome, RunOutcome, StepOutcome, Usage};

/// Set while a full-screen UI (the TUI) owns the terminal. Any diagnostic
/// output that isn't routed through a `Reporter` — ad hoc `eprintln!`s
/// scattered through resolution/search code — must check this before
/// writing, or it lands on top of the alternate screen and corrupts it.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// `eprintln!`, suppressed while [`set_quiet`] is in effect. Use for ad hoc
/// diagnostic lines that aren't structured `Event`s and so can't reach the
/// TUI as anything else — the alternative is losing them entirely while the
/// TUI is active.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        if !$crate::sink::is_quiet() {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("{}\n", $crate::sink::printable(&format!($($arg)*))),
            );
        }
    };
}

/// `text` with control characters removed, keeping newlines and tabs.
///
/// Text printed to the terminal can come from upstream: a build log line or
/// a package's own message. An escape sequence in it (a title change, a
/// cursor move, OSC 52 clipboard writes) would act on the user's terminal, so
/// C0, DEL and C1 controls, ESC included, are dropped before printing.
pub fn printable(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(is_stripped_control) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(strip_controls(text, false))
}

fn is_stripped_control(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}

/// [`printable`], except that SGR colour sequences (`ESC [ <digits> m`) are
/// kept. The human sink colours its own output with them; upstream text can
/// still set a colour, which is harmless, but cannot move the cursor, set a
/// title or write the clipboard.
fn strip_controls(text: &str, keep_sgr: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if keep_sgr && c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            while let Some(&p) = chars.peek() {
                if !(p.is_ascii_digit() || p == ';') {
                    break;
                }
                params.push(p);
                chars.next();
            }
            if chars.peek() == Some(&'m') {
                chars.next();
                out.push_str("\u{1b}[");
                out.push_str(&params);
                out.push('m');
            }
            continue;
        }
        if !is_stripped_control(c) {
            out.push(c);
        }
    }
    out
}

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

/// Events a view must not miss: each one flips a row to a final or
/// next-stage state, so a drop leaves it stale for the rest of the run
/// (a package stuck "building" after it finished). Queue-backed views
/// (the TUI) must block until there is room for these, and may drop everything
/// else (progress ticks, command and agent chatter) under load since later
/// events supersede them.
pub fn is_lifecycle(event: &Event) -> bool {
    matches!(
        event,
        Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::PackageFinished { .. }
            | Event::TargetSelected { .. }
            | Event::RunFinished { .. }
    )
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

thread_local! {
    /// Package name attached to events emitted by the currently-running
    /// reporter call. Written by [`PkgContextReporter`] for the duration of
    /// a single `report()`, and consumed by [`JsonlSink`] so the serialized
    /// line can carry a `pkg` even though the underlying `Event` variants
    /// only carry a per-package `step: StepId`.
    ///
    /// The 2026-10-06 external review flagged that `StepId`s are unique
    /// *within* a package but two parallel package threads both start at
    /// `StepId(0)`, so a JSONL stream with interleaved events was
    /// un-attributable without this. Field-level attribution on each `Event`
    /// variant was the other option but was ruled out because it would have
    /// required editing all ~124 `Event::*` construction sites across the
    /// workspace (several of them in files owned by other parallel agents).
    static CURRENT_PKG: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// RAII guard that sets [`CURRENT_PKG`] for the current thread and restores
/// the previous value on drop. Used by [`PkgContextReporter`] so nested
/// reporter calls (e.g. a `Tapped` TUI reporter forwarding to a tagged base)
/// pop back to the surrounding pkg instead of leaking `None`.
struct PkgGuard {
    previous: Option<String>,
}

impl PkgGuard {
    fn enter(pkg: Option<String>) -> Self {
        let previous = CURRENT_PKG.with(|p| {
            let prev = p.borrow().clone();
            *p.borrow_mut() = pkg;
            prev
        });
        Self { previous }
    }
}

impl Drop for PkgGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        CURRENT_PKG.with(|p| *p.borrow_mut() = prev);
    }
}

/// Returns the pkg name currently set on this thread via a
/// [`PkgContextReporter`], or `None` if no such context is active.
pub fn current_pkg() -> Option<String> {
    CURRENT_PKG.with(|p| p.borrow().clone())
}

/// Wraps a `Reporter` so every event it forwards is implicitly tagged with
/// `pkg` for the duration of the `report()` call. Lets JSONL/memory sinks
/// read the pkg via [`current_pkg`] without the `Event` enum itself needing
/// a per-variant `pkg` field.
pub struct PkgContextReporter {
    inner: std::sync::Arc<dyn Reporter + Send + Sync>,
    pkg: Option<String>,
}

impl PkgContextReporter {
    pub fn new(inner: std::sync::Arc<dyn Reporter + Send + Sync>, pkg: Option<String>) -> Self {
        Self { inner, pkg }
    }
}

impl Reporter for PkgContextReporter {
    fn report(&self, event: &Event) {
        let _guard = PkgGuard::enter(self.pkg.clone());
        self.inner.report(event);
    }
}

/// Sends every event to every sink. A warning whose text was already reported
/// is dropped: the same relaxed-bound note came 78 times in one run, and the
/// same git tag clobber warning 133 times, and each distinct warning needs
/// saying once per run.
pub struct FanoutReporter {
    sinks: Vec<Box<dyn Reporter>>,
    warned: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl FanoutReporter {
    pub fn new(sinks: Vec<Box<dyn Reporter>>) -> Self {
        Self {
            sinks,
            warned: Default::default(),
        }
    }

    /// Whether this warning text is new to the run, and is remembered as seen.
    fn first_time(&self, message: &str) -> bool {
        self.warned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(message.to_string())
    }
}

impl Reporter for FanoutReporter {
    fn report(&self, event: &Event) {
        if let Event::Warning { message, .. } = event
            && !self.first_time(message)
        {
            return;
        }
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
            | Event::StepSkipped { .. }
            | Event::SpecChanged { .. }
            | Event::PackageFinished { .. }
            | Event::NotesUsage { .. }
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
            Event::RunStarted {
                run_id, work_dir, ..
            } => Some(format!(
                "{} · run {} · {}",
                "auto-package".if_supports_color(Stderr, |t| t.bold()),
                run_id.if_supports_color(Stderr, |t| t.dimmed()),
                work_dir.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::RunFinished {
                outcome,
                elapsed,
                usage,
            } => {
                let outcome_colored = match outcome {
                    RunOutcome::Success => format!(
                        "{}",
                        "✓ run finished"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    _ => format!(
                        "{}",
                        "✗ run failed"
                            .if_supports_color(Stderr, |t| t.style(Style::new().red().bold()))
                    ),
                };
                Some(format!(
                    "\n{outcome_colored} in {} · {}",
                    fmt_elapsed(*elapsed),
                    format!("${:.4}", usage.cost_usd).if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::ResolveStarted { root } => Some(format!(
                "{} resolve {}",
                "◆".if_supports_color(Stderr, |t| t.cyan()),
                root.if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::ResolveFinished {
                total,
                built,
                in_distro,
                remaining,
            } => Some(format!(
                "  {} packages in closure · {} already built · {} in the distro · {} remaining",
                total,
                built.if_supports_color(Stderr, |t| t.green()),
                in_distro.if_supports_color(Stderr, |t| t.dimmed()),
                remaining,
            )),
            Event::TargetSelected {
                pkg,
                version,
                blocked_by,
                ..
            } => {
                let mut s = format!(
                    "\n{} {} {}",
                    "▶".if_supports_color(Stderr, |t| t.style(Style::new().cyan().bold())),
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
            Event::PackageFinished {
                pkg,
                version,
                outcome,
            } => {
                let outcome_colored = match outcome {
                    PackageOutcome::Built => format!(
                        "{}",
                        "✓ built"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    PackageOutcome::AlreadyBuilt => format!(
                        "{}",
                        "✔ already built".if_supports_color(Stderr, |t| t.green())
                    ),
                    PackageOutcome::MissingDeps { deps } => format!(
                        "{}",
                        format!("◇ blocked on {}", deps.join(", "))
                            .if_supports_color(Stderr, |t| t.yellow()),
                    ),
                    PackageOutcome::Failed { reason } => format!(
                        "{}",
                        format!("✗ failed: {reason}")
                            .if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
                    ),
                };
                Some(format!(
                    "{outcome_colored}  {} {}",
                    pkg.if_supports_color(Stderr, |t| t.bold()),
                    version.if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::StepStarted { kind, .. } => Some(format!(
                "  {} {}",
                "▸".if_supports_color(Stderr, |t| t.cyan()),
                kind.label().if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::StepSkipped { kind, reason } => Some(format!(
                "  {}",
                format!("⇥ skipped {}: {reason}", kind.label())
                    .if_supports_color(Stderr, |t| t.yellow()),
            )),
            Event::StepFinished {
                outcome, elapsed, ..
            } => {
                let outcome_colored = match outcome {
                    StepOutcome::Ok => format!("{}", "✓".if_supports_color(Stderr, |t| t.green())),
                    StepOutcome::Failed => {
                        format!("{}", "✗ failed".if_supports_color(Stderr, |t| t.red()))
                    }
                    StepOutcome::Skipped => {
                        format!("{}", "⇥ skipped".if_supports_color(Stderr, |t| t.yellow()))
                    }
                };
                Some(format!(
                    "    {outcome_colored} {}",
                    fmt_elapsed(*elapsed).if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::BuildFailed {
                classification,
                log,
                ..
            } => {
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
                    "  {} {label}\n    log: {}",
                    "✗ FAILED".if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
                    log.display().if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::Progress { message, .. } => Some(format!(
                "     {}",
                message.if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::Warning { message, .. } => Some(format!(
                "  {} {message}",
                "⚠ warning:".if_supports_color(Stderr, |t| t.style(Style::new().yellow().bold())),
            )),
            Event::MissingPackages { names, .. } => Some(format!(
                "  {} {}",
                "⚠ missing packages:".if_supports_color(Stderr, |t| t.yellow()),
                names.join(", "),
            )),
            Event::SpecChanged {
                lines_added,
                lines_removed,
                diff,
                artifact,
                ..
            } => {
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
            Event::Command {
                program,
                argv,
                cwd,
                log,
                ..
            } => {
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
            Event::ExtractResult {
                extract, artifact, ..
            } => {
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
            Event::AgentReply {
                response,
                usage,
                raw,
                artifact,
                ..
            } => {
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
                    _ => format!("{}", response.action.if_supports_color(Stderr, |t| t.red())),
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
                    format!("${:.4}", usage.cost_usd).if_supports_color(Stderr, |t| t.dimmed()),
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

/// A duration for people: `850ms`, `12.3s`, `4m07s`, `1h02m`.
fn fmt_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        0..1000 => format!("{ms}ms"),
        1000..60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        60_000..3_600_000 => format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60),
        _ => format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60),
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
            let _ = writeln!(std::io::stderr(), "{}", strip_controls(&msg, true));
        }
    }
}

/// The open file and when it was last flushed.
struct JsonlWriter {
    out: BufWriter<File>,
    last_flush: Instant,
}

/// Longest the tail of run.jsonl may sit in the buffer while events keep
/// arriving.
const JSONL_FLUSH_EVERY: Duration = Duration::from_secs(1);

/// Every open [`JsonlSink`]'s writer, so [`flush_jsonl_sinks`] can reach them
/// from a path that ends the process without dropping them.
static JSONL_WRITERS: Mutex<Vec<Weak<Mutex<JsonlWriter>>>> = Mutex::new(Vec::new());

/// Flush every open run.jsonl. Call before `process::exit`, which skips the
/// buffers' drop: events are only flushed on a timer and on terminal events.
pub fn flush_jsonl_sinks() {
    let writers = JSONL_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    for w in writers.iter().filter_map(Weak::upgrade) {
        let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.out.flush();
        w.last_flush = Instant::now();
    }
}

/// Start, once per process, the thread that flushes every run.jsonl each
/// [`JSONL_FLUSH_EVERY`]. The per-event timer only fires when another event
/// arrives, so without this a quiet stretch (a long build wait) leaves the
/// tail buffered, and a SIGKILL or panic loses it. The thread is detached and
/// sleeps between rounds, so it neither keeps the process alive nor delays
/// its exit.
fn start_jsonl_flusher() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("jsonl-flush".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(JSONL_FLUSH_EVERY);
                    flush_jsonl_sinks();
                }
            });
        // Without it the timer-on-event flush still applies.
        drop(spawned);
    });
}

pub struct JsonlSink {
    writer: Arc<Mutex<JsonlWriter>>,
    /// Set after the first write failure so the diagnostic prints once, not
    /// once per event.
    warned: AtomicBool,
    /// Likewise for an event that could not be serialized.
    serialize_warned: AtomicBool,
}

impl JsonlSink {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let writer = Arc::new(Mutex::new(JsonlWriter {
            out: BufWriter::new(file),
            last_flush: Instant::now(),
        }));
        let mut writers = JSONL_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
        writers.retain(|w| w.strong_count() > 0);
        writers.push(Arc::downgrade(&writer));
        drop(writers);
        start_jsonl_flusher();
        Ok(Self {
            writer,
            warned: AtomicBool::new(false),
            serialize_warned: AtomicBool::new(false),
        })
    }
}

/// JSONL line format. Wrapping the event in an envelope lets us carry
/// per-package attribution — set by the ambient [`PkgContextReporter`] —
/// alongside the event without touching the `Event` enum's variants (and
/// therefore without having to update every one of its ~124 construction
/// sites across the workspace, several of which live in files owned by
/// other parallel agents).
#[derive(Serialize)]
struct JsonlEnvelope<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pkg: Option<String>,
    #[serde(flatten)]
    event: &'a Event,
}

impl Reporter for JsonlSink {
    fn report(&self, event: &Event) {
        let envelope = JsonlEnvelope {
            pkg: current_pkg(),
            event,
        };
        let line = match serde_json::to_string(&envelope) {
            Ok(line) => line,
            Err(e) => {
                // The event is missing from the log, so say so, once.
                if !self.serialize_warned.swap(true, Ordering::Relaxed) {
                    diag!(
                        "warning: run.jsonl could not record an event ({e}); events may be missing"
                    );
                }
                return;
            }
        };
        // A panic in another thread mid-write poisons the lock; the
        // buffer is still usable, so keep logging.
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        // Flushed on events that close something out and at most a second
        // apart otherwise, not per event. The CLI's exit paths call
        // `process::exit`, which skips the BufWriter's drop, so they call
        // [`flush_jsonl_sinks`] first.
        let terminal = matches!(
            event,
            Event::PackageFinished { .. } | Event::RunFinished { .. } | Event::Warning { .. }
        );
        let due = terminal || w.last_flush.elapsed() >= JSONL_FLUSH_EVERY;
        let res = writeln!(w.out, "{line}").and_then(|()| {
            if due {
                w.last_flush = Instant::now();
                w.out.flush()
            } else {
                Ok(())
            }
        });
        if let Err(e) = res
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            diag!("warning: run.jsonl write failed ({e}); events may be missing");
        }
    }
}

/// What the CLI's summaries need from a run, not every event. A long run emits
/// many events, and keeping them all only to add up a few fields grew without
/// bound. So this keeps running usage totals and the `PackageFinished` events.
pub struct MemorySink {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    usage: Usage,
    finished: Vec<Event>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MemoryState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tokens and cost of every agent reply and notes turn so far.
    pub fn usage(&self) -> Usage {
        self.lock().usage.clone()
    }

    /// The `PackageFinished` events so far, in order.
    pub fn finished(&self) -> Vec<Event> {
        self.lock().finished.clone()
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for MemorySink {
    fn report(&self, event: &Event) {
        match event {
            Event::AgentReply { usage, .. } | Event::NotesUsage { usage, .. } => {
                let mut st = self.lock();
                st.usage.input_tokens += usage.input_tokens;
                st.usage.output_tokens += usage.output_tokens;
                st.usage.cost_usd += usage.cost_usd;
            }
            Event::PackageFinished { .. } => self.lock().finished.push(event.clone()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{StepId, StepKind};
    use std::io::{BufRead, BufReader};
    use std::sync::Arc;

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "ap-sink-test-{tag}-{nanos}-{}.jsonl",
            std::process::id()
        ))
    }

    /// With no later event to trigger the per-event timer, the background
    /// flusher still gets a buffered line to disk.
    #[test]
    fn a_warning_is_reported_once_per_run() {
        use std::sync::{Arc, Mutex};
        struct Count(Arc<Mutex<usize>>);
        impl Reporter for Count {
            fn report(&self, _: &Event) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let n = Arc::new(Mutex::new(0));
        let fan = FanoutReporter::new(vec![Box::new(Count(n.clone()))]);
        let warn = |m: &str| Event::Warning {
            step: None,
            message: m.to_string(),
        };
        for _ in 0..5 {
            fan.report(&warn("relaxed bound a"));
        }
        fan.report(&warn("relaxed bound b"));
        assert_eq!(*n.lock().unwrap(), 2);
    }

    #[test]
    fn a_quiet_stretch_still_flushes_the_tail() {
        let path = tmp_path("quiet");
        let sink = JsonlSink::open(&path).unwrap();
        sink.report(&Event::StepStarted {
            step: StepId(0),
            kind: StepKind::Build,
            attempt: 1,
            round: 0,
            detail: None,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(&path).unwrap().is_empty() {
            assert!(Instant::now() < deadline, "tail was never flushed");
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jsonl_envelope_includes_current_pkg() {
        let path = tmp_path("pkg");
        let sink = JsonlSink::open(&path).unwrap();
        let base: Arc<dyn Reporter + Send + Sync> = Arc::new(sink);
        let tagged = PkgContextReporter::new(base.clone(), Some("python-foo".into()));

        tagged.report(&Event::StepStarted {
            step: StepId(0),
            kind: StepKind::Build,
            attempt: 1,
            round: 0,
            detail: None,
        });

        // After the call, the thread-local must be restored.
        assert_eq!(current_pkg(), None);

        flush_jsonl_sinks();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("\"pkg\":\"python-foo\""),
            "jsonl line missing pkg attribution: {contents}"
        );
        assert!(contents.contains("\"event\":\"step_started\""));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn jsonl_write_failure_warns_once_and_does_not_panic() {
        let sink = JsonlSink::open(Path::new("/dev/full")).unwrap();
        assert!(!sink.warned.load(Ordering::Relaxed));
        // A warning is flushed straight away, so the failure shows at once.
        let warning = Event::Warning {
            step: None,
            message: "x".into(),
        };
        sink.report(&warning);
        assert!(sink.warned.load(Ordering::Relaxed));
        sink.report(&warning);
    }

    #[test]
    fn memory_sink_survives_a_poisoned_lock() {
        let sink = Arc::new(MemorySink::new());
        let s2 = sink.clone();
        let _ = std::thread::spawn(move || {
            let _g = s2.state.lock().unwrap();
            panic!("poison");
        })
        .join();
        sink.report(&Event::PackageFinished {
            pkg: "x".into(),
            version: "1".into(),
            outcome: PackageOutcome::Built,
        });
        assert_eq!(sink.finished().len(), 1);
    }

    #[test]
    fn memory_sink_keeps_usage_totals_and_finished_packages_only() {
        let sink = MemorySink::new();
        let usage = Usage {
            input_tokens: 3,
            output_tokens: 4,
            cost_usd: 0.5,
        };
        sink.report(&Event::ResolveStarted { root: "x".into() });
        sink.report(&Event::NotesUsage {
            step: crate::event::StepId(1),
            usage: usage.clone(),
        });
        sink.report(&Event::NotesUsage {
            step: crate::event::StepId(2),
            usage,
        });
        assert_eq!(sink.finished().len(), 0);
        let total = sink.usage();
        assert_eq!((total.input_tokens, total.output_tokens), (6, 8));
        assert!((total.cost_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jsonl_envelope_omits_pkg_when_no_context() {
        let path = tmp_path("nopkg");
        let sink = JsonlSink::open(&path).unwrap();
        // Call JsonlSink directly, with no ambient pkg context.
        sink.report(&Event::ResolveStarted { root: "x".into() });
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("\"pkg\""),
            "unexpected pkg field in untagged line: {contents}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parallel_tagged_streams_do_not_cross_contaminate() {
        // Two threads each emit a batch of events through their own tagged
        // reporter, writing into a shared JsonlSink. Afterwards, every line
        // in the file must still carry its correct pkg, and ordering within
        // either thread's own slice doesn't matter.
        let path = tmp_path("parallel");
        let sink: Arc<dyn Reporter + Send + Sync> = Arc::new(JsonlSink::open(&path).unwrap());

        let per_thread = 50usize;
        let handles: Vec<_> = ["pkg-a", "pkg-b"]
            .iter()
            .map(|name| {
                let base = sink.clone();
                let name = name.to_string();
                std::thread::spawn(move || {
                    let r = PkgContextReporter::new(base, Some(name.clone()));
                    for _ in 0..per_thread {
                        r.report(&Event::StepStarted {
                            step: StepId(0),
                            kind: StepKind::Build,
                            attempt: 1,
                            round: 0,
                            detail: None,
                        });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        flush_jsonl_sinks();
        let file = std::fs::File::open(&path).unwrap();
        let mut a = 0usize;
        let mut b = 0usize;
        let mut other = 0usize;
        for line in BufReader::new(file).lines() {
            let line = line.unwrap();
            if line.contains("\"pkg\":\"pkg-a\"") {
                a += 1;
            } else if line.contains("\"pkg\":\"pkg-b\"") {
                b += 1;
            } else {
                other += 1;
            }
        }
        assert_eq!(a, per_thread, "pkg-a count wrong, raw other={other}");
        assert_eq!(b, per_thread, "pkg-b count wrong, raw other={other}");
        assert_eq!(other, 0, "every line should carry a pkg tag");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nested_contexts_restore_outer_pkg_on_drop() {
        // Simulates a `Tapped` reporter forwarding through a tagged base:
        // the outer tagged reporter should still see its own pkg after the
        // inner call returns.
        let outer = PkgContextReporter::new(
            Arc::new(NullReporter) as Arc<dyn Reporter + Send + Sync>,
            Some("outer".into()),
        );
        let inner = PkgContextReporter::new(
            Arc::new(NullReporter) as Arc<dyn Reporter + Send + Sync>,
            Some("inner".into()),
        );

        // Fabricate an event delivery that nests inner inside outer.
        struct Probe {
            seen: Mutex<Vec<Option<String>>>,
            inner: Arc<dyn Reporter + Send + Sync>,
        }
        impl Reporter for Probe {
            fn report(&self, event: &Event) {
                self.seen.lock().unwrap().push(current_pkg());
                self.inner.report(event);
                // After the inner call, the ambient pkg must still match ours.
                self.seen.lock().unwrap().push(current_pkg());
            }
        }
        let probe = Probe {
            seen: Mutex::new(Vec::new()),
            inner: Arc::new(inner),
        };
        let probe_arc: Arc<dyn Reporter + Send + Sync> = Arc::new(probe);
        let outer2 = PkgContextReporter::new(probe_arc.clone(), Some("outer".into()));
        outer2.report(&Event::ResolveStarted { root: "r".into() });
        let _ = outer; // silence unused warning

        // Can't downcast the Arc<dyn Reporter> to Probe easily; just verify
        // the ambient pkg was fully torn down after the whole call ends.
        assert_eq!(current_pkg(), None);
    }
}

#[cfg(test)]
mod printable_tests {
    use super::*;

    #[test]
    fn escape_sequences_from_upstream_are_not_printed() {
        // An OSC title change and a clipboard write, as a build log might carry.
        let text = "ok\u{1b}]0;pwned\u{7}\n\tnext\u{1b}]52;c;ZGF0YQ==\u{1b}\\";
        let out = printable(text);
        assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
        assert_eq!(out, "ok]0;pwned\n\tnext]52;c;ZGF0YQ==\\");
    }

    #[test]
    fn c1_controls_are_dropped_too() {
        // U+009B is the single-character CSI.
        assert_eq!(printable("a\u{9b}2Jb"), "a2Jb");
    }

    #[test]
    fn plain_text_is_borrowed_unchanged() {
        assert!(matches!(
            printable("plain\ttext\n"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn human_output_keeps_its_own_colours_only() {
        let msg = "\u{1b}[31mred\u{1b}[0m \u{1b}[2J\u{1b}]0;x\u{7}";
        let out = strip_controls(msg, true);
        assert_eq!(out, "\u{1b}[31mred\u{1b}[0m J]0;x");
    }
}

#[cfg(test)]
mod elapsed_tests {
    use super::*;

    #[test]
    fn durations_read_at_their_own_scale() {
        assert_eq!(fmt_elapsed(Duration::from_millis(850)), "850ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(12_340)), "12.3s");
        assert_eq!(fmt_elapsed(Duration::from_secs(247)), "4m07s");
        assert_eq!(fmt_elapsed(Duration::from_secs(3720)), "1h02m");
    }
}
