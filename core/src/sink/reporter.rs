use super::*;

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
            | Event::PackageDropped { .. }
            | Event::SweepChecking { .. }
            // The answer that closes the row SweepChecking opened: a drop
            // would leave that row `checking`.
            | Event::SweepProgress { .. }
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
    /// `StepId`s are unique only within a package, so parallel packages both
    /// start at `StepId(0)`; the `pkg` is what attributes an interleaved line.
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
