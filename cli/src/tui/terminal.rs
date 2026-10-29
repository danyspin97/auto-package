use super::*;

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

/// Set once [`Screen::open`] has taken the terminal over (raw mode is on). A
/// run whose TUI never started leaves it clear, so its guards write nothing:
/// stdout may be a file, and an escape sequence there would be garbage.
pub(super) static TERMINAL_TAKEN: AtomicBool = AtomicBool::new(false);

/// Runs `restore` only when `taken` is set, that is when the terminal was
/// actually taken over.
pub(super) fn restore_if_taken(taken: &AtomicBool, restore: impl FnOnce()) {
    if taken.load(Ordering::SeqCst) {
        restore();
    }
}

impl TerminalGuard {
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_if_taken(&TERMINAL_TAKEN, || {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        });
    }
}
