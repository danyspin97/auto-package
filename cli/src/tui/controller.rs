use super::*;

/// What a run is about, shown in the summary header: the OBS project it
/// builds in and the packages it was asked for. No `targets` means the whole
/// project.
#[derive(Clone, Default)]
pub struct RunScope {
    pub project: String,
    pub targets: Vec<String>,
    /// How many packages build at once (`--jobs`), which explains a queue
    /// longer than that; 0 for no limit.
    pub jobs: usize,
}

/// Handle used by the CLI to obtain reporters that feed the TUI.
///
/// Cheap to clone; every clone shares the same channel to the render thread.
/// The run's reporter, once [`TuiController::set_recorder`] gave it.
pub type RecorderSlot = std::sync::Arc<Mutex<Option<std::sync::Arc<dyn Reporter + Send + Sync>>>>;

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
    /// The names rows are known by, keyed by the row's RPM name (see
    /// [`RowNames`]).
    names: std::sync::Arc<Mutex<RowNames>>,
    /// The error that ended the render loop early, if it did (see
    /// [`TuiController::failure`]).
    failure: std::sync::Arc<Mutex<Option<String>>>,
    /// Set once the render loop has died before the run closed, so events
    /// from then on are printed as plain output (see [`RenderDeath`]).
    death: std::sync::Arc<RenderDeath>,
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
    /// What reads diffs and makes submit requests (see [`TuiController::set_backend`]).
    backend: BackendSlot,
    /// The run's reporter, for the events the TUI itself causes (see
    /// [`TuiController::set_recorder`]).
    recorder: RecorderSlot,
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
        let death = std::sync::Arc::new(RenderDeath::default());
        let thread_death = death.clone();
        let add_tx = accepts_requests.then_some(add_tx);
        // The render thread reports whether it could take the terminal over
        // before it does anything else, so a failure comes back here, with
        // nothing printed yet.
        let (ready_tx, ready_rx) = sync_channel::<io::Result<()>>(1);
        let backend: BackendSlot = std::sync::Arc::new(Mutex::new(None));
        let thread_backend = backend.clone();
        let recorder: RecorderSlot = std::sync::Arc::new(Mutex::new(None));
        let thread_recorder = recorder.clone();
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
                thread_recorder,
                screen,
                rx,
                add_tx,
                thread_work,
                memory,
                scope,
                thread_settled,
                thread_backend,
            ) {
                eprintln!("TUI error: {e}");
                *thread_failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                // After `run` returned, so the terminal is already restored.
                thread_death.mark_died();
                // Release quiet now rather than waiting for the next tapped event:
                // events reported before one arrives, or with none coming at all,
                // would otherwise be dropped. `should_switch` stays once-only, so
                // a later call only re-clears an already-cleared flag.
                ap_core::sink::set_quiet(false);
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
            names: std::sync::Arc::new(Mutex::new(RowNames::default())),
            failure,
            death,
            work,
            settled,
            idle: std::sync::Arc::new((Mutex::new(()), Condvar::new())),
            render: std::sync::Arc::new(Mutex::new(Some(render))),
            backend,
            recorder,
        })
    }

    /// The run's reporter. The TUI records what it causes through it, such as
    /// a host the user allows, so the run's log has it.
    pub fn set_recorder(&self, reporter: std::sync::Arc<dyn Reporter + Send + Sync>) {
        *self.recorder.lock().unwrap_or_else(|p| p.into_inner()) = Some(reporter);
    }

    /// Let the TUI read the diff of a package against its origin and make
    /// submit requests through `backend` (the `d`, `S` and `A` keys). With
    /// `hold_open` the TUI stays open once every row is built, so a request can
    /// still be made; `q` closes it.
    pub fn set_backend(&self, backend: std::sync::Arc<dyn PackageBackend>, hold_open: bool) {
        *self.backend.lock().unwrap_or_else(|p| p.into_inner()) = Some((backend, hold_open));
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
        self.with_names(|n| n.note_origin(row, request));
    }

    /// The name `row` was requested under, if one was recorded. Rows a
    /// build did not start from a request (dependencies, for one) have none.
    pub fn origin_of(&self, row: &str) -> Option<String> {
        self.with_names(|n| n.origin_of(row).map(str::to_string))
    }

    /// Record the package name a dependency row `row` was built for, so a
    /// failure of that name can be settled by the row (see
    /// `withdraw_settled_failures`). Not a request, so a retry never resends it.
    pub fn note_label(&self, row: &str, label: &str) {
        self.with_names(|n| n.note_label(row, label));
    }

    /// The package name recorded for `row` by [`TuiController::note_label`].
    pub fn label_of(&self, row: &str) -> Option<String> {
        self.with_names(|n| n.label_of(row).map(str::to_string))
    }

    fn with_names<T>(&self, f: impl FnOnce(&mut RowNames) -> T) -> T {
        f(&mut self.names.lock().unwrap_or_else(|p| p.into_inner()))
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
            death: self.death.clone(),
        })
    }
}
