use super::*;

/// A request for a new root build, sent to the run by the TUI.
pub enum AddRequest {
    /// A package name or repository URL typed into the `n` prompt.
    New(String),
    /// A retry of the row with this key (its RPM name), sent under the name
    /// the row was requested with (see [`TuiController::origin_of`]).
    Retry(String),
}

pub(super) struct Msg {
    pub(super) pkg: Option<String>,
    pub(super) event: Event,
}

/// Whether the render loop has died before the run closed, and whether the
/// run has already switched back to plain output because of it.
///
/// The render thread calls [`RenderDeath::mark_died`] once it has restored the
/// terminal. The event forwarding (see `Tapped`) calls
/// [`RenderDeath::should_switch`] on every event, and that returns `true` once,
/// so the switch (`set_quiet(false)`) happens exactly once.
#[derive(Default)]
pub struct RenderDeath {
    died: AtomicBool,
    switched: AtomicBool,
}

impl RenderDeath {
    /// Record that the render loop died mid-run.
    pub fn mark_died(&self) {
        self.died.store(true, Ordering::SeqCst);
    }

    /// `true` on the first call after the render loop died, `false` otherwise.
    pub fn should_switch(&self) -> bool {
        self.died.load(Ordering::SeqCst)
            && self
                .switched
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }
}

/// The plain-text sink, attached whether or not the run shows the TUI. While a
/// TUI owns the terminal it is quiet (see `ap_core::sink::set_quiet`), so this
/// prints nothing (a warning is held for the summary); once the render loop
/// dies and [`RenderDeath::should_switch`] clears quiet, it prints the rest of
/// the run.
pub struct PlainOutput {
    inner: ap_core::sink::HumanSink,
}

impl PlainOutput {
    pub fn new(inner: ap_core::sink::HumanSink) -> Self {
        Self { inner }
    }
}

impl Reporter for PlainOutput {
    fn report(&self, event: &Event) {
        if ap_core::sink::is_quiet() {
            // A warning is held for the summary after the TUI; the run log has every event.
            if let Event::Warning { message, .. } = event {
                ap_core::sink::hold_warning(message);
            }
            return;
        }
        self.inner.report(event);
    }
}

pub(super) struct Tapped {
    pub(super) base: std::sync::Arc<dyn Reporter + Send + Sync>,
    pub(super) pkg: Option<String>,
    pub(super) tx: SyncSender<Msg>,
    pub(super) dropped: std::sync::Arc<AtomicUsize>,
    pub(super) death: std::sync::Arc<RenderDeath>,
}

impl Reporter for Tapped {
    fn report(&self, event: &Event) {
        // The render loop died mid-run: nobody is drawing the TUI any more, so
        // release the quiet flag (once) and let the plain sink print from here on.
        if self.death.should_switch() {
            ap_core::sink::set_quiet(false);
        }
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
