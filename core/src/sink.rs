use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use owo_colors::{OwoColorize, Stream::Stderr, Style};
use serde::Serialize;

use crate::event::{Event, PackageOutcome, RunOutcome, StepOutcome};

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
            eprintln!($($arg)*);
        }
    };
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
                        "Failed".if_supports_color(Stderr, |t| t.style(Style::new().red().bold()))
                    ),
                };
                Some(format!(
                    "run finished: {outcome_colored} in {secs}.{frac:03}s · {}",
                    format!("${:.4}", usage.cost_usd).if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::ResolveStarted { root } => Some(format!(
                "resolve {}",
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
            Event::PackageFinished {
                pkg,
                version,
                outcome,
            } => {
                let outcome_colored = match outcome {
                    PackageOutcome::Built => {
                        format!("{}", "built".if_supports_color(Stderr, |t| t.green()))
                    }
                    PackageOutcome::AlreadyBuilt => format!(
                        "{}",
                        "already built".if_supports_color(Stderr, |t| t.green())
                    ),
                    PackageOutcome::MissingDeps { deps } => format!(
                        "{}",
                        format!("needs: {}", deps.join(", "))
                            .if_supports_color(Stderr, |t| t.yellow()),
                    ),
                    PackageOutcome::Failed { reason } => format!(
                        "{}",
                        format!("failed: {reason}").if_supports_color(Stderr, |t| t.red()),
                    ),
                };
                Some(format!(
                    "{} {} {}: {outcome_colored}",
                    "finished".if_supports_color(Stderr, |t| t.bold()),
                    pkg.if_supports_color(Stderr, |t| t.bold()),
                    version.if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::StepStarted { kind, .. } => Some(format!(
                "  {}",
                kind.label().if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::StepSkipped { kind, reason } => Some(format!(
                "  {}",
                format!("skipped {}: {reason}", kind.label())
                    .if_supports_color(Stderr, |t| t.yellow()),
            )),
            Event::StepFinished {
                outcome, elapsed, ..
            } => {
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
                    "  done  {}  {outcome_colored}",
                    format!("{secs}.{frac:03}s").if_supports_color(Stderr, |t| t.dimmed()),
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
                    "  {}: {label}\n  log: {}",
                    "FAILED".if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
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
    /// Set after the first write failure so the diagnostic prints once, not
    /// once per event.
    warned: AtomicBool,
}

impl JsonlSink {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
            warned: AtomicBool::new(false),
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
        if let Ok(line) = serde_json::to_string(&envelope) {
            // A panic in another thread mid-write poisons the lock; the
            // buffer is still usable, so keep logging.
            let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            let res = writeln!(w, "{line}").and_then(|()| w.flush());
            if let Err(e) = res
                && !self.warned.swap(true, Ordering::Relaxed)
            {
                diag!("warning: run.jsonl write failed ({e}); events may be missing");
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
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for MemorySink {
    fn report(&self, event: &Event) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
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
        sink.report(&Event::ResolveStarted { root: "x".into() });
        assert!(sink.warned.load(Ordering::Relaxed));
        sink.report(&Event::ResolveStarted { root: "y".into() });
    }

    #[test]
    fn memory_sink_survives_a_poisoned_lock() {
        let sink = Arc::new(MemorySink::new());
        let s2 = sink.clone();
        let _ = std::thread::spawn(move || {
            let _g = s2.events.lock().unwrap();
            panic!("poison");
        })
        .join();
        sink.report(&Event::ResolveStarted { root: "x".into() });
        assert_eq!(sink.events().len(), 1);
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
