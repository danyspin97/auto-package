use super::*;

/// The terminal the render thread draws on, and the guard that restores it.
pub(super) struct Screen {
    pub(super) terminal: Terminal<CrosstermBackend<io::Stdout>>,
    _restore: TerminalGuard,
}

impl Screen {
    /// Takes over the terminal, or fails having changed nothing that needs
    /// undoing. Stdout must be a terminal: raw mode can succeed on
    /// `/dev/tty` even when stdout is redirected, and then the TUI would
    /// draw into a file. The guard is armed only once raw mode is on, so a
    /// failure before that writes no escape sequences.
    pub(super) fn open() -> io::Result<Self> {
        if !io::stdout().is_terminal() {
            return Err(io::Error::other("stdout is not a terminal"));
        }
        enable_raw_mode()?;
        TERMINAL_TAKEN.store(true, Ordering::SeqCst);
        let restore = TerminalGuard::new();
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        // Without this, the alternate screen keeps whatever happened to be in its
        // buffer when we switched to it (shell scrollback on terminals that mirror
        // the primary buffer, previous-run leftovers on terminals that preserve
        // the alt buffer across invocations). Ratatui's `draw` only sends diffs
        // against its own previous buffer — not the terminal's — so cells we
        // render as default (space + default style) silently keep that leftover
        // content showing through. One hard clear up front wipes the slate.
        terminal.clear()?;
        Ok(Self {
            terminal,
            _restore: restore,
        })
    }
}

/// The names rows are known by, keyed by the row's RPM name. Two maps with two
/// uses, kept apart so that one never answers for the other:
///
/// - `origins` holds the name a root build was requested under. A retry resends
///   it, so it must only ever hold names that a build takes as a request.
/// - `labels` holds the package name a dependency row was built for. Only the
///   withdrawal of failures reads it; a retry never does, since `foo` as a
///   request is a different package from the dependency `python-foo`.
#[derive(Default)]
pub struct RowNames {
    origins: HashMap<String, String>,
    labels: HashMap<String, String>,
}

impl RowNames {
    /// The first request name recorded for `row` is kept.
    pub fn note_origin(&mut self, row: &str, request: &str) {
        self.origins
            .entry(row.to_string())
            .or_insert_with(|| request.to_string());
    }

    pub fn origin_of(&self, row: &str) -> Option<&str> {
        self.origins.get(row).map(String::as_str)
    }

    /// The first label recorded for `row` is kept.
    pub fn note_label(&mut self, row: &str, label: &str) {
        self.labels
            .entry(row.to_string())
            .or_insert_with(|| label.to_string());
    }

    pub fn label_of(&self, row: &str) -> Option<&str> {
        self.labels.get(row).map(String::as_str)
    }
}
