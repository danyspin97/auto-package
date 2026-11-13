//! The session around a run: the interrupt handler and the wait for the
//! terminal to be restored, the exit of the TUI, and the start of the TUI.

use super::*;

/// The agent cannot work (claude is not logged in), found before a run
/// starts. `main` exits with `EXIT_AGENT_UNAVAILABLE` for it.
#[derive(Debug)]
pub struct AgentUnavailable(pub String);

impl std::fmt::Display for AgentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AgentUnavailable {}

/// A run that submitted nothing on purpose: refused before it started because
/// it would submit with no terminal to confirm on, or a `--yes` batch refused
/// for a change that needs a review. `main` exits with `EXIT_NOT_SUBMITTED`.
#[derive(Debug)]
pub struct NotSubmitted(pub String);

impl std::fmt::Display for NotSubmitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotSubmitted {}

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
        // The wait is over: the same final lines, without waiting for the writes.
        print_exit_lines(0, false, Some(&memory.usage()));
        std::process::exit(130);
    }
    // Stop the build workers (own process groups, so the terminal's Ctrl-C
    // never reached them): otherwise `osc`/`claude` outlive this process.
    // Sets the shutdown flag first, so polling loops and the scheduler stop
    // on their next check. A baseline restore is spared: it puts OBS back.
    // The interrupt is recorded first: a failure the kill causes reads it.
    ap_build::proc::request_interrupt();
    ap_build::proc::kill_all_groups();
    // The TUI leaves raw mode and the alternate screen on; restore them before
    // printing anything, or it lands in that state. Done before the wait, so
    // a second Ctrl-C is a signal again and not a keypress nobody reads.
    if tui_active {
        tui::restore_terminal();
    }
    let writing = ap_build::proc::obs_writes_in_flight();
    if writing > 0 {
        ap_core::sink::print_line(&obs_wait_line(RESTORE_WAIT.as_secs(), writing));
    }
    let timed_out = writing > 0 && !ap_build::proc::wait_for_obs_writes(RESTORE_WAIT);
    ap_build::proc::remove_private_dir();
    ap_build::upstream_cache::flush();
    ap_core::sink::flush_jsonl_sinks();
    print_exit_lines(writing, timed_out, Some(&memory.usage()));
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

/// Prints the lines that end a run, once the terminal is back. `writes` is how
/// many OBS writes were in flight when the wait began and `timed_out` whether
/// they outlived it. The held count is read now, so it includes everything held
/// up to the exit.
pub(crate) fn print_exit_lines(
    writes: usize,
    timed_out: bool,
    interrupted: Option<&ap_core::event::Usage>,
) {
    let held = ap_core::sink::held_summary();
    let waited = tui::builds_at_close();
    for line in exit_lines(
        writes,
        timed_out,
        interrupted,
        (held.0, held.1.as_deref()),
        waited,
    ) {
        ap_core::sink::eprint_coloured(&line);
    }
}

/// The lines that end a run, in order: the OBS warning when a write was in
/// flight and did not finish, the interruption, the held count, and last the
/// builds the run waited for after the TUI closed (`waited`, 0 for none). Each
/// is one line. The OBS wait line is printed before the wait (see `obs_wait_line`).
pub(crate) fn exit_lines(
    writes: usize,
    timed_out: bool,
    interrupted: Option<&ap_core::event::Usage>,
    held: (usize, Option<&std::path::Path>),
    waited: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    if writes > 0 && timed_out {
        out.push(
            "warning: an OBS write did not finish; the next run of that package restores it first"
                .to_string(),
        );
    }
    if let Some(usage) = interrupted {
        out.push(format!(
            "Interrupted — tokens: {} in / {} out — cost: ${:.2}",
            usage.input_tokens, usage.output_tokens, usage.cost_usd
        ));
    }
    if let Some(line) = ap_core::sink::held_line(held.0, held.1) {
        out.push(line);
    }
    if waited > 0 {
        let noun = if waited == 1 { "build" } else { "builds" };
        out.push(format!(
            "waited for {waited} {noun} still running when the TUI closed; they finished before exit"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logged_out_agent_is_found_under_a_context() {
        let report = color_eyre::Report::new(AgentUnavailable("log in".into())).wrap_err("run");
        assert!(report.downcast_ref::<AgentUnavailable>().is_some());
    }

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
    fn the_exit_lines_come_in_order_and_each_is_one_line() {
        let usage = ap_core::event::Usage {
            input_tokens: 440,
            output_tokens: 165605,
            cost_usd: 6.34,
            ..Default::default()
        };
        let path = std::path::Path::new("/w/run/diagnostics.log");
        let lines = exit_lines(1, true, Some(&usage), (2, Some(path)), 0);
        assert_eq!(
            lines,
            vec![
                "warning: an OBS write did not finish; the next run of that package restores it first".to_string(),
                "Interrupted — tokens: 440 in / 165605 out — cost: $6.34".to_string(),
                "2 messages held while the TUI was open, see /w/run/diagnostics.log".to_string(),
            ]
        );
        assert!(lines.iter().all(|l| !l.contains('\n')));
        // A write that finished in time adds no warning; nothing held, no line.
        let lines = exit_lines(1, false, Some(&usage), (0, None), 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Interrupted"));
        assert!(exit_lines(0, false, None, (0, None), 0).is_empty());
        let lines = exit_lines(0, false, None, (0, None), 2);
        assert_eq!(
            lines,
            vec![
                "waited for 2 builds still running when the TUI closed; they finished before exit"
                    .to_string()
            ]
        );
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
        ap_core::sink::eprint_coloured(&format!(
            "warning: the TUI stopped before the run ended ({e})"
        ));
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
            ap_core::sink::eprint_coloured(&format!(
                "warning: cannot show the TUI ({e}); printing plain output instead"
            ));
            None
        }
    }
}
