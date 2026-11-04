//! Diagnostic lines that reach the terminal outside the event stream: the
//! `diag!` lines and the warnings of the plain sink while the TUI owns the
//! screen. A line is printed once, however often it is offered; while the TUI
//! is up it is held, and the terminal gets one summary after it is restored.
//! Every line is also appended to the run's diagnostics file.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{LogLevel, is_quiet, printable};

static DIAG: Mutex<Option<Diagnostics>> = Mutex::new(None);

/// What became of a line offered to [`Diagnostics`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Identical to a line already offered (printed, held or recorded).
    Repeat,
    /// Written to the diagnostics file only: a debug line that does not print.
    Recorded,
    /// Held for the summary, because the TUI owns the terminal.
    Held,
    /// To be printed now.
    Print(String),
}

#[derive(Default)]
pub(crate) struct Diagnostics {
    /// Every ordinary line offered so far, so a repeat is dropped.
    pub(crate) seen: HashSet<String>,
    /// Keys of the debug lines already offered (one per key).
    pub(crate) keys: HashSet<String>,
    /// Lines held while the TUI owned the terminal, in the order first seen.
    pub(crate) held: Vec<String>,
    /// The run's diagnostics file, once the run has one.
    pub(crate) file: Option<PathBuf>,
    /// Lines recorded before the file was attached; written to it on attach.
    pub(crate) early: Vec<String>,
    /// Whether debug lines print (`--log debug` or finer).
    pub(crate) debug: bool,
}

impl Diagnostics {
    /// Offers an ordinary line: printed now, or held while `quiet` (the TUI
    /// owns the terminal). An identical line is dropped.
    pub(crate) fn offer(&mut self, line: &str, quiet: bool) -> Offer {
        if !self.seen.insert(line.to_string()) {
            return Offer::Repeat;
        }
        self.record(line);
        if quiet {
            self.held.push(line.to_string());
            Offer::Held
        } else {
            Offer::Print(line.to_string())
        }
    }

    /// Offers a debug line under `key`: the first line for a key is recorded,
    /// and printed only at debug level and never while `quiet`.
    pub(crate) fn offer_debug(&mut self, key: &str, line: &str, quiet: bool) -> Offer {
        if !self.keys.insert(key.to_string()) {
            return Offer::Repeat;
        }
        self.record(line);
        if debug_prints(self.debug, quiet) {
            Offer::Print(line.to_string())
        } else {
            Offer::Recorded
        }
    }

    /// Appends a line to the diagnostics file, or keeps it until one is attached.
    fn record(&mut self, line: &str) {
        match &self.file {
            Some(path) => append_line(path, line),
            None => self.early.push(line.to_string()),
        }
    }

    /// Points the diagnostics at `path` and writes the lines recorded before it.
    pub(crate) fn attach_file(&mut self, path: &Path) {
        for line in std::mem::take(&mut self.early) {
            append_line(path, &line);
        }
        self.file = Some(path.to_path_buf());
    }
}

fn append_line(path: &Path, line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Whether a debug line prints: only at debug level, and never under the TUI.
pub(crate) fn debug_prints(debug: bool, quiet: bool) -> bool {
    debug && !quiet
}

/// The summary line for the held lines, or `None` when there are none.
pub fn held_line(count: usize, file: Option<&Path>) -> Option<String> {
    if count == 0 {
        return None;
    }
    let s = if count == 1 { "" } else { "s" };
    Some(match file {
        Some(file) => format!(
            "{count} message{s} held while the TUI was open, see {}",
            file.display()
        ),
        None => format!("{count} message{s} held while the TUI was open"),
    })
}

fn with<T>(f: impl FnOnce(&mut Diagnostics) -> T) -> T {
    let mut guard = DIAG.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Diagnostics::default))
}

/// Points the diagnostics at the run's file; lines are appended to it.
pub fn set_diagnostics_file(path: &Path) {
    with(|d| d.attach_file(path));
}

/// Sets whether debug lines print (`--log debug` and finer).
pub fn set_log_level(level: LogLevel) {
    with(|d| d.debug = level >= LogLevel::Debug);
}

/// Prints an ordinary diagnostic line, or holds it while the TUI is up. The
/// line is printed at most once per process.
pub fn emit(line: &str) {
    let line = printable(line);
    let quiet = is_quiet();
    if let Offer::Print(line) = with(|d| d.offer(&line, quiet)) {
        eprintln!("{line}");
    }
}

/// Offers a debug line for `key`: recorded once in the diagnostics file, and
/// printed once only at debug level and never under the TUI.
pub fn debug_once(key: &str, line: &str) {
    let line = printable(line);
    let quiet = is_quiet();
    if let Offer::Print(line) = with(|d| d.offer_debug(key, &line, quiet)) {
        eprintln!("{line}");
    }
}

/// How many lines were held while the TUI owned the terminal, and the file
/// they are also written to.
pub fn held_summary() -> (usize, Option<PathBuf>) {
    with(|d| (d.held.len(), d.file.clone()))
}
