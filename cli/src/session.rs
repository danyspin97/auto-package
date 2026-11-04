//! The session around a run: the interrupt handler and the wait for the
//! terminal to be restored, the exit of the TUI, and the start of the TUI.

use super::*;

/// The longest an interrupted run waits for the OBS writes still in progress
/// (package runs, restores, update commits). A restore is one `osc addremove`
/// and one `osc ci`, each capped by the restore timeout, so this covers a
/// restore that runs to its cap. Past it the process exits: a baseline that was
/// not put back stays on disk marked pending, and the next run of that package
/// restores it before writing.
const RESTORE_WAIT: std::time::Duration =
    std::time::Duration::from_secs(2 * ap_build::proc::RESTORE_TIMEOUT.as_secs() + 30);

/// Clean process-wide shutdown for Ctrl-C — restore the terminal if the TUI
/// had it, print token usage so far, and actually end the process.
///
/// Called from two places: the `ctrlc` crate's real-SIGINT handler (no TUI,
/// or a TUI that for some reason isn't in raw mode) and, directly, from the
/// TUI's own key-handling loop. Raw mode disables signal generation for
/// Ctrl-C entirely, so while the TUI is running the OS never delivers SIGINT
/// at all — Ctrl-C only reaches the process as an ordinary
/// `KeyCode::Char('c')` + `CONTROL` keypress, which is why the TUI cannot
/// just rely on the handler below firing and must call this itself. Either
/// way this has to actually terminate the whole process, not merely stop
/// rendering — the pipeline's package-build threads keep running otherwise,
/// unaffected by whatever the TUI's own event loop does.
pub(super) fn handle_interrupt(tui_active: bool, memory: &MemorySink) -> ! {
    // A second interrupt while the first one waits for restores ends the wait.
    static INTERRUPTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if INTERRUPTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(130);
    }
    // Stop the build workers (own process groups, so the terminal's Ctrl-C
    // never reached them): otherwise `osc`/`claude` outlive this process.
    // Sets the shutdown flag first, so polling loops and the scheduler stop
    // on their next check. A baseline restore is spared: it puts OBS back.
    ap_build::proc::kill_all_groups();
    // The TUI leaves raw mode and the alternate screen on; restore them before
    // printing anything, or it lands in that state. Done before the wait, so
    // a second Ctrl-C is a signal again and not a keypress nobody reads.
    if tui_active {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    }
    let writing = ap_build::proc::obs_writes_in_flight();
    if writing > 0 {
        eprintln!("{}", obs_wait_line(RESTORE_WAIT.as_secs(), writing));
        if !ap_build::proc::wait_for_obs_writes(RESTORE_WAIT) {
            eprintln!(
                "warning: an OBS write did not finish: OBS may keep a failed attempt; \
                 its saved baseline stays in the work directory marked pending, and the \
                 next run of that package restores it first"
            );
        }
    }
    ap_build::proc::remove_private_dir();
    ap_build::upstream_cache::flush();
    ap_core::sink::flush_jsonl_sinks();
    let usage = memory.usage();
    for line in after_tui_lines(Some(&usage), ap_core::sink::held_summary()) {
        eprintln!("{line}");
    }
    // The shell convention for death by SIGINT; scripts can tell an
    // interrupted run from a failed one.
    std::process::exit(130);
}

/// The line that says an interrupted run is waiting for OBS writes. One line,
/// printed once the terminal is back.
pub(super) fn obs_wait_line(secs: u64, writes: usize) -> String {
    let noun = if writes == 1 { "write" } else { "writes" };
    format!(
        "waiting up to {secs} s for {writes} OBS {noun} to finish; \
         Ctrl-C again stops waiting"
    )
}

/// The lines printed once the terminal is back: the interruption, when the run
/// was interrupted, and then the count of messages held while the TUI was open
/// (see `ap_core::sink::held_summary`). Each is one line.
pub(crate) fn after_tui_lines(
    interrupted: Option<&ap_core::event::Usage>,
    held: (usize, Option<std::path::PathBuf>),
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(usage) = interrupted {
        out.push(format!(
            "Interrupted — tokens: {} in / {} out — cost: ${:.2}",
            usage.input_tokens, usage.output_tokens, usage.cost_usd
        ));
    }
    if let Some(line) = ap_core::sink::held_line(held.0, held.1.as_deref()) {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_obs_wait_is_one_line_with_the_count_and_the_seconds() {
        assert_eq!(
            obs_wait_line(270, 1),
            "waiting up to 270 s for 1 OBS write to finish; Ctrl-C again stops waiting"
        );
        assert_eq!(
            obs_wait_line(270, 2),
            "waiting up to 270 s for 2 OBS writes to finish; Ctrl-C again stops waiting"
        );
        assert!(!obs_wait_line(270, 2).contains('\n'));
    }

    #[test]
    fn after_the_tui_an_interruption_and_the_held_count_are_two_lines_at_most() {
        let usage = ap_core::event::Usage {
            input_tokens: 440,
            output_tokens: 165605,
            cost_usd: 6.34,
            ..Default::default()
        };
        let lines = after_tui_lines(Some(&usage), (2, Some("/w/run/diagnostics.log".into())));
        assert_eq!(
            lines,
            vec![
                "Interrupted — tokens: 440 in / 165605 out — cost: $6.34".to_string(),
                "2 messages held while the TUI was open, see /w/run/diagnostics.log".to_string(),
            ]
        );
        assert!(after_tui_lines(None, (0, None)).is_empty());
    }
}

/// Route Ctrl-C (and, with ctrlc's `termination` feature, SIGTERM/SIGHUP) to
/// [`handle_interrupt`]. A later call (`--maintenance` runs `update` once per
/// code stream) points the installed handler at the new run instead of failing.
pub(super) fn install_interrupt_handler(
    tui_active: bool,
    memory: Arc<MemorySink>,
) -> color_eyre::Result<()> {
    type Current = std::sync::Mutex<Option<(bool, Arc<MemorySink>)>>;
    static CURRENT: Current = std::sync::Mutex::new(None);
    let first = CURRENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace((tui_active, memory))
        .is_none();
    if !first {
        return Ok(());
    }
    ctrlc::set_handler(|| {
        let (tui_active, memory) = CURRENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("set before the handler");
        // Off this thread: it waits for restores, and a second Ctrl-C must
        // still reach the handler to end that wait.
        std::thread::spawn(move || {
            handle_interrupt(tui_active, &memory);
        });
    })
    .map_err(|e| color_eyre::eyre::eyre!("failed to install the interrupt handler: {e}"))
}

/// How a run that was shown in the TUI ends, shared by `build` and `update`.
///
/// `settled` is the TUI's verdict ([`tui::TuiController::all_settled`]), read
/// once it has closed. A settled run has every package built, so it exits 0
/// only if its own run succeeded and no failure is left standing. `failures`
/// holds one message per failure that no later build fixed: a retry that
/// built the package has already removed its earlier failure, and a request
/// that failed before it had a row has no row to retry, so it stays here.
/// Any other run stays open until the user quits, and exits with the first
/// error it has: the run's own, then the failed packages', or else the run
/// ended before every package was built.
///
/// `tui_failure` is set when the render loop died before the run closed. No
/// one was watching the run then, so it is judged by what its builds returned,
/// as a plain run would be, instead of as an unsettled TUI run.
pub(super) fn tui_run_exit(
    settled: bool,
    tui_failure: Option<String>,
    run: color_eyre::Result<()>,
    failures: Vec<String>,
) -> color_eyre::Result<()> {
    if settled {
        return run_and_failures(run, failures);
    }
    if let Some(e) = tui_failure {
        eprintln!("warning: the TUI stopped before the run ended ({e})");
        return run_and_failures(run, failures);
    }
    if run.is_err() || !failures.is_empty() {
        return run_and_failures(run, failures);
    }
    Err(color_eyre::eyre::eyre!(
        "the run ended before every package was built"
    ))
}

/// The run's own error if it has one, else every failure joined into one
/// error, else `Ok`.
fn run_and_failures(run: color_eyre::Result<()>, failures: Vec<String>) -> color_eyre::Result<()> {
    run?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(color_eyre::eyre::eyre!(failures.join("; ")))
    }
}

/// The TUI for a run that asked for it, or `None` when it cannot be shown
/// here (no terminal to take over). Then a warning says so, and the run goes
/// on as a plain one, with text output and no `--tui`.
pub(super) fn start_tui(
    enable: bool,
    memory: Arc<MemorySink>,
    scope: tui::RunScope,
    accepts_requests: bool,
) -> Option<tui::TuiController> {
    if !enable {
        return None;
    }
    match tui::TuiController::new(memory, scope, accepts_requests) {
        Ok(tui) => Some(tui),
        Err(e) => {
            eprintln!("warning: cannot show the TUI ({e}); printing plain output instead");
            None
        }
    }
}
