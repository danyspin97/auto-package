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

/// The builds still running when the TUI closed. A run waits for them before it
/// returns (see `run::run`), so the exit line can say how many it waited for.
static BUILDS_AT_CLOSE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Records how many builds were still running when the TUI closed.
pub(super) fn set_builds_at_close(builds: usize) {
    BUILDS_AT_CLOSE.store(builds, Ordering::SeqCst);
}

/// How many builds were still running when the TUI closed (0 when none were).
pub fn builds_at_close() -> usize {
    BUILDS_AT_CLOSE.load(Ordering::SeqCst)
}

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
        restore_terminal();
    }
}

/// The bytes that leave the alternate screen and show the cursor, in the order
/// they are written.
pub(crate) fn restore_sequence() -> Vec<u8> {
    let mut out = Vec::new();
    let _ = crossterm::queue!(
        out,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    out
}

/// Puts the terminal back as the shell had it: raw mode off, then the
/// alternate screen left and the cursor shown, flushed before anything else is
/// printed. Does nothing when the TUI never took the terminal.
pub fn restore_terminal() {
    restore_if_taken(&TERMINAL_TAKEN, || {
        let _ = disable_raw_mode();
        use std::io::Write as _;
        let mut out = io::stdout();
        let _ = out.write_all(&restore_sequence());
        let _ = out.flush();
    });
}
