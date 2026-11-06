use super::*;

/// Runs the event loop on an open `screen`. The screen's guard restores the
/// terminal on every way out of this function — clean exit, a render or input
/// error, or a panic unwinding through here. (Ctrl-C is the one exit path that
/// does NOT come back through here at all — `crate::handle_interrupt` does its
/// own equivalent teardown and then ends the process on the spot.)
pub(super) fn run(
    recorder: RecorderSlot,
    mut screen: Screen,
    rx: Receiver<Msg>,
    add_tx: Option<Sender<AddRequest>>,
    work: std::sync::Arc<AtomicUsize>,
    memory: std::sync::Arc<MemorySink>,
    scope: RunScope,
    settled: std::sync::Arc<AtomicBool>,
    backend: BackendSlot,
) -> io::Result<()> {
    let result = event_loop(
        &mut screen.terminal,
        rx,
        add_tx.as_ref(),
        &work,
        &memory,
        scope,
        &settled,
        &backend,
        &recorder,
    );
    let _ = screen.terminal.show_cursor();
    drop(screen);
    set_builds_at_close(work.load(Ordering::SeqCst));
    // The TUI can be left with builds still running: the user quit once the
    // run was over but added a package afterwards (see `run_over`). Those builds
    // run on with the terminal given back, and quiet stays on for them: nothing
    // from them is printed until the process ends, when the held lines are
    // printed (see `session::print_exit_lines`). The run log has every event.
    result
}

/// Whether every package the run was asked for is settled, so the TUI can
/// close by itself and the run exits 0.
///
/// A package is settled once its own build has finished with a built outcome
/// (`Built` or `AlreadyBuilt`) and no retry is asked for. Nothing else settles
/// a row: one parked on missing deps is not settled, even once its dependencies
/// are built, and neither is a failed row.
///
/// `work_left` counts the builds still running (see
/// [`TuiController::work_finished`]), so a run with work outstanding is never
/// settled. Rows are all checked rather than only the scope's targets, since
/// the targets are typed names and rows use rpm names, so matching them up is
/// not reliable.
///
/// Rows that are only what the sweep's check found are left out: nothing was
/// queued for them. A run with no other rows is settled only once it has ended successfully:
/// `RunFinished` is reported after the root work is released, so until then a
/// `build` that has not shown its first row yet stays open, while an `update`
/// with nothing to do closes at once. A failure before anything started stays
/// on screen.
pub(super) fn run_is_done(state: &TuiState, work_left: usize) -> bool {
    if work_left != 0 {
        return false;
    }
    let mut rows = state.build_rows().peekable();
    if rows.peek().is_none() {
        return matches!(state.run_outcome, Some(RunOutcome::Success));
    }
    rows.all(|p| p.finished && !p.retry_requested && p.status.is_built())
}

/// Whether the run has nothing left to do, built or not: `work_left` is zero
/// and every row has finished with no retry asked for. Such a run is over but
/// may not be settled (see [`run_is_done`]); then the TUI stays open so the
/// user can add a package, retry one, or quit. A row parked on missing deps
/// counts as finished here, since its attempt has ended and nothing will
/// re-queue it once `work_left` is zero.
pub(super) fn run_is_over(state: &TuiState, work_left: usize) -> bool {
    let mut rows = state.build_rows().peekable();
    work_left == 0 && rows.peek().is_some() && rows.all(|p| p.finished && !p.retry_requested)
}

/// The footer line for a run that is over but not settled, so the user knows
/// what to do next. `None` while the run is still going and once it is
/// settled, since the TUI is about to close itself then. Only a run that
/// accepts requests offers `n`.
pub(super) fn over_hint(over: bool, settled: bool, accepts_requests: bool) -> Option<&'static str> {
    if !over || settled {
        return None;
    }
    Some(if accepts_requests {
        "Run over with packages not built: press n to add a package, q to quit"
    } else {
        "Run over with packages not built: press q to quit"
    })
}

/// Hand `name` to the run as a new root build. It is counted as outstanding
/// work before it is sent, so the run cannot look finished in the gap between
/// the request leaving and its build starting. `false` if the run is gone.
pub(super) fn request_build(
    add_tx: &Sender<AddRequest>,
    work: &AtomicUsize,
    req: AddRequest,
) -> bool {
    work.fetch_add(1, Ordering::SeqCst);
    let sent = add_tx.send(req).is_ok();
    if !sent {
        work.fetch_sub(1, Ordering::SeqCst);
    }
    sent
}

/// The longest a frame spends applying queued events before it draws.
const DRAIN_BUDGET: Duration = Duration::from_millis(50);

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: Receiver<Msg>,
    add_tx: Option<&Sender<AddRequest>>,
    work: &AtomicUsize,
    memory: &MemorySink,
    scope: RunScope,
    settled: &AtomicBool,
    backend_slot: &BackendSlot,
    recorder: &RecorderSlot,
) -> io::Result<()> {
    let (job_tx, job_rx) = channel::<Job>();
    let mut state = TuiState::new(scope);
    state.accepts_requests = add_tx.is_some();
    // The allowlist the proxy and the source checks read; the allow keys add to it.
    let hosts = ap_build::allowed_hosts::shared();
    // `Some` while the "add package" prompt is open and capturing keys.
    let mut input: Option<String> = None;
    let mut table_state = TableState::default();
    // Tracked by package name rather than raw table index: `visible_order()`
    // shrinks whenever a package finishes, which would otherwise shift every
    // later row up and silently hand the highlight to a different package.
    let mut selected: Option<String> = None;
    // Scroll state for the selected package. It belongs to `browse_for`, the
    // package it was last synced to: a key press can change `selected` before
    // the next frame, so comparing against the previous frame's selection
    // would miss the change.
    let mut browse = BrowseState::default();
    let mut browse_for: Option<String> = None;
    let tick_rate = Duration::from_millis(150);
    let mut finished_at: Option<Instant> = None;
    let mut senders_dropped = false;
    // Read fresh at each use: a key press can add work partway through a frame.
    let run_over = |state: &TuiState| {
        state.run_outcome.is_some() || run_is_over(state, work.load(Ordering::SeqCst))
    };

    loop {
        let (backend, hold_open) = match backend_slot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            Some((b, hold)) => (Some(b.clone()), *hold),
            None => (None, false),
        };
        state.can_submit = backend.is_some();
        while let Ok(job) = job_rx.try_recv() {
            modal::apply_job(&mut state, &mut browse, job);
        }
        // Drain everything currently queued so a burst of events (e.g. many
        // packages starting at once) renders as one frame, not one per event.
        // Bounded by time, so producers that never pause can't starve the draw
        // and input below — a resize would otherwise never be picked up.
        let drain_until = Instant::now() + DRAIN_BUDGET;
        while Instant::now() < drain_until {
            match rx.try_recv() {
                Ok(msg) => state.apply(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    senders_dropped = true;
                    finished_at.get_or_insert(Instant::now());
                    break;
                }
            }
        }

        {
            let visible = state.visible_order();
            if selected
                .as_deref()
                .is_none_or(|s| !visible.iter().any(|n| n.as_str() == s))
            {
                selected = visible.first().map(|n| (*n).clone());
            }
            table_state.select(
                selected
                    .as_ref()
                    .and_then(|s| visible.iter().position(|n| n.as_str() == s)),
            );
        }
        // Lines that arrived since the last frame push a scrolled view up so
        // it keeps showing the same content; a view at the tail follows.
        let log_seq = selected
            .as_deref()
            .and_then(|s| state.packages.get(s))
            .map_or(0, |p| p.log_seq);
        if selected != browse_for {
            browse.reset_for_package(log_seq);
            browse_for = selected.clone();
        } else {
            browse.log_off = follow_adjust(browse.log_off, log_seq, browse.log_seen);
            browse.log_seen = log_seq;
        }

        let done = run_is_done(&state, work.load(Ordering::SeqCst));
        let hint = if hold_open && done {
            Some(
                "Run done: S submit selected \u{2502} A submit all built \u{2502} d diff \u{2502} e explain \u{2502} q quit",
            )
        } else {
            over_hint(run_over(&state), done, state.accepts_requests)
        };
        let mut feedback = RenderFeedback::default();
        terminal.draw(|f| {
            feedback = render(f, &state, &mut table_state, input.as_deref(), &browse, hint);
        })?;
        // Keep the offset within what rendering can show, or PageDown
        // appears dead after holding PageUp at the top.
        browse.log_off = feedback.log_off;
        browse.act_top = feedback.act_top;
        browse.detail_off = feedback.detail_off;

        if event::poll(tick_rate)?
            && let TermEvent::Key(key) = event::read()?
        {
            // The prompt swallows every key except Ctrl-C, so typing a name
            // containing `q`, `j`, `a`... doesn't quit or move the selection.
            if let Some(buf) = input.as_mut()
                && !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL))
            {
                match prompt_key(buf, key.code) {
                    PromptAction::Pending => {}
                    PromptAction::Cancel => input = None,
                    PromptAction::Submit(name) => {
                        input = None;
                        if add_tx.is_some_and(|tx| {
                            request_build(tx, work, AddRequest::New(name.clone()))
                        }) {
                            state.add_global_log(
                                LogLevel::Info,
                                format!("Queued {name} as a new root package"),
                            );
                        } else {
                            state.add_global_log(
                                LogLevel::Error,
                                format!(
                                    "Cannot add {name}: the run is no longer accepting packages"
                                ),
                            );
                        }
                    }
                }
                continue;
            }
            // A modal takes every key but Ctrl-C.
            if browse.modal.is_some()
                && !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL))
            {
                match &mut browse.modal {
                    Some(Modal::Diff(v)) => match modal::diff_key(v, key.code) {
                        modal::DiffAction::None => {}
                        modal::DiffAction::Close => browse.modal = None,
                        action => {
                            modal::redo_diff(&state, &mut browse, action, backend.as_ref(), &job_tx)
                        }
                    },
                    Some(Modal::Submit(m)) => match modal::submit_key(m, key.code) {
                        modal::SubmitAction::None => {}
                        modal::SubmitAction::Close => browse.modal = None,
                        modal::SubmitAction::Confirm => modal::confirm_submit(
                            &mut state,
                            &mut browse,
                            backend.as_ref(),
                            &job_tx,
                        ),
                    },
                    Some(Modal::Explain(v)) => {
                        if modal::explain_key(v, key.code) == modal::ExplainAction::Close {
                            browse.modal = None;
                        }
                    }
                    Some(Modal::AllowHost(m)) => {
                        let action = modal::allow_host_key(m, key.code);
                        modal::apply_allow_action(&mut state, m, action, &hosts, recorder);
                        if m.is_finished() {
                            browse.modal = None;
                        }
                    }
                    None => {}
                }
                continue;
            }
            // The `/` prompt captures keys the same way the add prompt does.
            if let Some(buf) = browse.filter_input.as_mut()
                && !(key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL))
            {
                match prompt_key(buf, key.code) {
                    PromptAction::Pending => {}
                    PromptAction::Submit(text) => {
                        browse.filter_input = None;
                        browse.filter = Some(text);
                        browse.log_off = 0;
                    }
                    PromptAction::Cancel => {
                        browse.filter_input = None;
                        // A blank Enter clears the filter; Esc leaves it be.
                        if key.code == KeyCode::Enter {
                            browse.filter = None;
                            browse.log_off = 0;
                        }
                    }
                }
                continue;
            }
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('+') if add_tx.is_some() => {
                    input = Some(String::new())
                }
                KeyCode::Char('/') if browse.focus == Focus::Log => {
                    browse.filter_input = Some(browse.filter.clone().unwrap_or_default());
                }
                KeyCode::Esc if esc_action(&browse) != EscAction::Quit => match esc_action(&browse)
                {
                    EscAction::ClosePrompt => browse.filter_input = None,
                    EscAction::ClearFilter => {
                        browse.filter = None;
                        browse.log_off = 0;
                    }
                    EscAction::Collapse => browse.act_expanded = false,
                    EscAction::Quit => {}
                },
                // Quit means quit: end the whole process (killing the running
                // builds), not just the UI. Detaching used to leave the
                // pipeline running headless with nothing on screen and no
                // way to stop it short of `kill`.
                // Once the run is over there is nothing to interrupt: leave
                // normally, so a successful run exits 0 and a failed one keeps
                // the exit code the CLI gives it.
                KeyCode::Char('q') | KeyCode::Esc => {
                    if run_over(&state) {
                        break;
                    }
                    crate::handle_interrupt(true, memory)
                }
                KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    if run_over(&state) {
                        break;
                    }
                    // Raw mode disables signal generation entirely, so the OS
                    // never delivers a real SIGINT while the TUI is running —
                    // this keypress IS the only place Ctrl-C shows up. Same
                    // shutdown as `q`/Esc above.
                    crate::handle_interrupt(true, memory);
                }
                KeyCode::Tab => browse.focus = cycle_focus(browse.focus, false),
                KeyCode::BackTab => browse.focus = cycle_focus(browse.focus, true),
                // In the table, the arrows move the package selection; in the
                // log pane they scroll it. The Activity pane has no keys yet.
                KeyCode::Down | KeyCode::Char('j') if browse.focus == Focus::Table => {
                    select_next(&mut selected, &state)
                }
                KeyCode::Up | KeyCode::Char('k') if browse.focus == Focus::Table => {
                    select_prev(&mut selected, &state)
                }
                // `h` opens the prompt that allows the refused hosts of the
                // selected package, while it has any still to allow. `a` and
                // `A` always keep their meanings (show succeeded, submit all).
                KeyCode::Char('h')
                    if modal::allow_target(&state, selected.as_deref(), &hosts).is_some() =>
                {
                    modal::open_allow(&state, &mut browse.modal, selected.as_deref(), &hosts);
                }
                KeyCode::Char('a') => state.show_all = !state.show_all,
                KeyCode::Char('d') => {
                    if let Some(row) = selected.as_deref() {
                        modal::open_diff(&state, &mut browse, row, backend.as_ref(), &job_tx);
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(row) = selected.as_deref() {
                        modal::open_explain(
                            &mut browse,
                            row,
                            modal::explain_loader().as_ref(),
                            &job_tx,
                        );
                    }
                }
                KeyCode::Char('S') | KeyCode::Char('A') => {
                    let all = key.code == KeyCode::Char('A');
                    let rows = modal::submit_rows(&state, selected.as_deref(), all);
                    if rows.is_empty() {
                        state.add_global_log(
                            LogLevel::Info,
                            "No built package without a submit request".to_string(),
                        );
                    } else {
                        modal::open_submit(
                            &mut state,
                            &mut browse,
                            rows,
                            backend.as_ref(),
                            &job_tx,
                        );
                    }
                }
                // Works in every focus: the lists replace the detail panes
                // wholesale, so there is no pane the key could belong to.
                KeyCode::Char('l') => browse.show_lists = !browse.show_lists,
                // The lists have no cursor, so they scroll from the top like
                // the expanded activity text, on the keys the log pane uses.
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                    if browse.show_lists && browse.focus == Focus::Log =>
                {
                    // Each list clamps against its own length, so the shorter
                    // one stops at its end while the longer keeps scrolling.
                    browse.queued_off = text_scroll_key(
                        key.code,
                        browse.queued_off,
                        feedback.queued_total,
                        feedback.lists_viewport,
                    );
                    browse.built_off = text_scroll_key(
                        key.code,
                        browse.built_off,
                        feedback.built_total,
                        feedback.lists_viewport,
                    );
                }
                // Retry a package that failed with "No space left on
                // device" (`PkgStatus::NoSpace`) once the user has freed
                // some — re-sent through the same channel as the `n` add
                // prompt, as a retry of this row. Any other status is a
                // no-op: there is nothing a disk cleanup would fix for a
                // real spec defect.
                // `r` retries a package that failed of "No space left on
                // device" or was refused a host now allowed (see `retry_key`).
                KeyCode::Char('r') => {
                    modal::retry_key(&mut state, add_tx, work, selected.as_deref(), &hosts)
                }
                // Event Log scrollback: pin to the live tail by default (0),
                // step back through history on demand without losing new
                // lines arriving underneath — they just wait below the
                // viewport until `End` jumps back to them.
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                    if browse.focus == Focus::Activity =>
                {
                    if browse.act_expanded {
                        browse.detail_off = text_scroll_key(
                            key.code,
                            browse.detail_off,
                            feedback.detail_total,
                            feedback.detail_viewport,
                        );
                    } else {
                        let sel = browse.act_sel.min(feedback.act_len.saturating_sub(1));
                        browse.act_sel =
                            activity_move(sel, feedback.act_len, key.code, feedback.act_viewport);
                    }
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    browse.log_off = scroll_key(
                        key.code,
                        browse.log_off,
                        feedback.log_total,
                        feedback.log_viewport,
                    );
                }
                KeyCode::Enter if browse.focus == Focus::Activity && feedback.act_len > 0 => {
                    // Freeze the selection on the entry being read: while it
                    // follows the newest entry, the text would change underneath.
                    browse.act_sel = browse.act_sel.min(feedback.act_len - 1);
                    browse.act_expanded = !browse.act_expanded;
                    browse.detail_off = 0;
                }
                _ => {}
            }
        }

        // A settled run gets a two-second grace so its final state can be read.
        // Work added during the grace (`n` or `r`) cancels it, and the timer
        // starts over once the run is settled again. A run that is over but not
        // settled stays open until the user quits.
        if run_is_done(&state, work.load(Ordering::SeqCst)) {
            let at = finished_at.get_or_insert(Instant::now());
            // With a backend that keeps the TUI open the user ends it with `q`.
            if !hold_open && at.elapsed() >= Duration::from_secs(2) {
                break;
            }
        } else if senders_dropped {
            // All senders are gone but no `RunFinished` ever arrived (early
            // abort before the run was announced, or the main thread
            // dropped its reporters). No more events can ever arrive — spin
            // only to let input finish and then exit so the terminal
            // teardown guard runs.
            break;
        } else {
            finished_at = None;
        }
    }

    settled.store(
        run_is_done(&state, work.load(Ordering::SeqCst)),
        Ordering::SeqCst,
    );
    Ok(())
}

pub(super) enum PromptAction {
    Pending,
    Cancel,
    Submit(String),
}

/// Apply one keypress to the "add package" prompt buffer.
pub(super) fn prompt_key(buf: &mut String, code: KeyCode) -> PromptAction {
    match code {
        KeyCode::Esc => PromptAction::Cancel,
        KeyCode::Enter => {
            let name = buf.trim().to_string();
            if name.is_empty() {
                PromptAction::Cancel
            } else {
                PromptAction::Submit(name)
            }
        }
        KeyCode::Backspace => {
            buf.pop();
            PromptAction::Pending
        }
        KeyCode::Char(c) => {
            buf.push(c);
            PromptAction::Pending
        }
        _ => PromptAction::Pending,
    }
}
