//! One turn: launch the program, read its stream with the watchdog and the
//! limits, and parse the answer into a turn outcome.

use super::*;
use crate::agent::build_prompt::*;

pub(super) fn turn_inner(req: TurnRequest<'_>) -> Result<TurnOutcome, Error> {
    let prompt = build_turn_prompt(&req);

    let mut allowed_tools = if req.task == Task::PatchReview {
        review_allowed_tools(req.workspace, req.checkout_dir, req.writable_patches)
    } else {
        turn_allowed_tools(req.task, req.workspace, req.checkout_dir)
    };
    if req.task != Task::TranslateUnresolvableDeps {
        allowed_tools.push_str(&rpm_inspection_tools(req.rpms_dir));
        allowed_tools.push_str(&sibling_read_tools(req.sibling_dirs));
    }
    let mut denied_tools = turn_denied_tools(req.workspace, req.checkout_dir);
    let mut service = super::owned_files::service_denied_tools(req.task, req.checkout_dir);
    if req.task == Task::PatchReview {
        service.extend(review_denied_tools(req.checkout_dir));
    }
    if !service.is_empty() {
        denied_tools.push(',');
        denied_tools.push_str(&service.join(","));
    }

    // Escalate with the attempt number: the first 2 attempts go to Haiku 5.5
    // (cheapest), the next 3 to Sonnet 5.5, and everything from attempt 6 on —
    // the packages the cheaper tiers already failed on — to Opus 5.5.
    let model = model_for_task(req.task, req.attempt, req.model_rotation);

    // Cap this turn at whatever is left of the package budget, so one runaway
    // turn cannot overshoot it — the caller only checks between turns.
    let remaining_usd = req.budget_usd - req.spent_usd;
    // Below the floor a turn cannot do anything useful and the old 1-cent
    // minimum overspent an exhausted budget; report it the way the CLI reports
    // its own cap, so the caller's budget handling applies.
    if remaining_usd < MIN_TURN_BUDGET_USD {
        return Err(Error::EndedEarly(Box::new(EndedEarlyError {
            subtype: "error_max_budget_usd".to_string(),
            message: format!(
                "only ${:.2} of the ${:.2} budget is left, below the ${MIN_TURN_BUDGET_USD:.2} \
                 a turn needs; no turn was started",
                remaining_usd.max(0.0),
                req.budget_usd
            ),
            stdout: String::new(),
            prompt,
            turns: 0,
            usage: Usage::default(),
        })));
    }

    // Sent via `--append-system-prompt` instead of folded into `prompt`
    // below — see `static_system_prompt`'s doc comment for why that's what
    // actually lets this ~140KB of unchanging instructions be served from
    // Anthropic's prompt cache instead of re-billed as fresh input on every
    // single turn, for every package.
    let system_prompt_file = SystemPromptFile::create().map_err(Error::Io)?;
    materialise_conventions(req.workspace).map_err(Error::Io)?;

    let mut cmd = Command::new(claude_program());
    // Scrub the inherited environment before applying any `.env(...)` call
    // below: `env_clear` drops the parent env, `envs(scrubbed)` adds back
    // only the allow-listed entries. See `scrub_env_for_agent` for the
    // rationale. Any extra `cmd.env(...)`
    // after this point — e.g. QUILT_PATCHES below — is additive and NOT
    // credential-shaped, so it is fine to set after the clear.
    //
    // Extra variables the operator named in AUTO_PACKAGE_AGENT_ENV_PASS are
    // added back by `agent_env`.
    let scrubbed_env = agent_env();
    cmd.env_clear().envs(&scrubbed_env);
    let schema = turn_response_schema(req.task);
    cmd.args([
        "--print",
        "--verbose",
        "--output-format",
        "stream-json",
        "--model",
        model,
        "--json-schema",
        schema.as_str(),
        "--add-dir",
        &req.workspace.to_string_lossy(),
        "--allowed-tools",
        &allowed_tools,
        "--disallowed-tools",
        &denied_tools,
        "--max-budget-usd",
        &format!("{remaining_usd:.4}"),
        "--max-turns",
        &max_agent_turns(req.task).to_string(),
        // Path rules in `--allowed-tools` are only enforced in the default
        // permission mode, and user settings could widen them, so the turn
        // runs with project settings only, as the notes turn does.
        "--permission-mode",
        "default",
        "--setting-sources",
        "project",
        // By file, not as an argument: a single argv entry is capped at
        // 131072 bytes (E2BIG, an opaque launch error) and this prompt is
        // within a few KB of that. The text — and so the prompt cache — is
        // identical either way.
        "--append-system-prompt-file",
        &system_prompt_file.path.to_string_lossy(),
        // The default system prompt's per-machine sections (cwd, env info,
        // git status) otherwise differ across packages' checkout
        // directories and would break cache-prefix reuse between them even
        // with a shared `--append-system-prompt`.
        "--exclude-dynamic-system-prompt-sections",
    ]);
    if let Some(dir) = skills_dir() {
        cmd.arg("--plugin-dir").arg(dir);
    }
    // The agent edits the spec through the checkout, not through the
    // workspace's `spec` symlink, so it needs access to both directories.
    if let Some(checkout) = req.checkout_dir {
        cmd.args(["--add-dir", &checkout.to_string_lossy()]);
    }
    cmd.envs(quilt_env(req.checkout_dir));
    if matches!(req.task, Task::RefreshPatches | Task::PatchReview) {
        cmd.env("QUILT_SERIES", REFRESH_SERIES);
    }
    if let Some(id) = req.session_id {
        cmd.args(["--resume", id]);
    }
    cmd.args(["-p", &prompt]);

    // Own process group so the watchdog (and cleanup after exit) can kill the
    // CLI's tool subprocesses too; they would otherwise keep the pipes open.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .current_dir(req.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(Error::Launch)?;
    let group = match crate::proc::track_group(child.id()) {
        Ok(g) => g,
        // Shutdown began before registration; the group is already killed.
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Launch(e));
        }
    };

    // Both are piped above; if either is missing the turn cannot be read, so
    // end the child rather than panic.
    let (child_stdout, child_stderr) = match (child.stdout.take(), child.stderr.take()) {
        (Some(o), Some(e)) => (o, e),
        _ => {
            crate::proc::kill_process_group(child.id());
            let _ = child.wait();
            return Err(Error::Io(std::io::Error::other(
                "claude CLI output pipes were not captured",
            )));
        }
    };

    // A turn that never ends (network stall, a tool waiting for input) would
    // otherwise block the stdout loop below forever; the per-package budget is
    // only checked between turns. Kill the whole group after the deadline.
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    // True until the CLI has exited. The watchdog signals the group only while
    // holding this lock and seeing it true, and the turn clears it before it
    // reaps the child, so the watchdog can never signal a group id that was
    // freed by the reap and handed to another process.
    let child_alive = std::sync::Arc::new(std::sync::Mutex::new(true));
    // Milliseconds since `started` at the last line of output. The watchdog
    // reads it; the stdout loop below writes it.
    let started = std::time::Instant::now();
    let last_output = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (turn_timeout, idle_timeout, watchdog_tick) = turn_deadlines();
    {
        let pid = child.id();
        let timed_out = timed_out.clone();
        let child_alive = child_alive.clone();
        let last_output = last_output.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            loop {
                match done_rx.recv_timeout(watchdog_tick) {
                    // Finished normally, or the turn's sender was dropped.
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let elapsed = started.elapsed();
                let idle = elapsed.saturating_sub(std::time::Duration::from_millis(
                    last_output.load(Ordering::SeqCst),
                ));
                if elapsed >= turn_timeout || idle >= idle_timeout {
                    let alive = child_alive.lock().unwrap_or_else(|e| e.into_inner());
                    if *alive {
                        timed_out.store(true, Ordering::SeqCst);
                        crate::proc::kill_process_group(pid);
                    }
                    return;
                }
            }
        });
    }

    // Collect stderr in a background thread so it doesn't block stdout reads.
    let stderr_rx = {
        let stderr = child_stderr;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            // `read_to_string` would fail outright on any invalid UTF-8 byte
            // and discard everything read — plausible for a crash that dumps
            // a native backtrace or truncates mid-codepoint. Reading raw
            // bytes and converting lossily means a crash-stderr.log is never
            // silently empty just because one byte in it wasn't UTF-8.
            let mut buf = Vec::new();
            let read = std::io::BufReader::new(stderr)
                .take(MAX_AGENT_STREAM_BYTES)
                .read_to_end(&mut buf);
            let mut text = String::from_utf8_lossy(&buf).into_owned();
            // Say so rather than leave a stderr.log that looks like "no output".
            if let Err(e) = read {
                text.push_str(&format!("\n[stderr read error: {e}]\n"));
            }
            let _ = tx.send(text);
        });
        rx
    };

    // Stream stdout line by line: display text/tool events and collect the
    // final `result` event which carries the structured JSON response.
    // `ResultEvent`/`StreamUsage` live at module scope (see below) so that
    // a schema fixture test can parse a real claude-CLI payload against
    // the exact same deserializer the production path uses — otherwise
    // a silent rename of `subtype`/`is_error`/`errors` on the CLI side
    // would downgrade this whole mechanism to the old opaque path with
    // no CI signal.
    let mut result_event = ResultEvent::default();
    let mut saw_result = false;
    let mut unparsed_lines = 0usize;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    // Usage each assistant message reported, keyed by message id: one message
    // is sent as several events, so keeping the last report per id avoids
    // counting it more than once.
    let mut message_usage: std::collections::BTreeMap<String, Usage> =
        std::collections::BTreeMap::new();
    // Raw copy of every stdout line, kept independent of JSON parsing so a
    // crash mid-stream (or an unparseable line) still leaves something to
    // inspect — the parsed path above silently drops anything it can't read.
    let mut stdout_raw = String::new();
    {
        let mut reader = std::io::BufReader::new(child_stdout);
        let mut raw_line: Vec<u8> = Vec::new();
        let mut read_errors = 0u32;
        // Give up only after this many errors in a row, so a persistent one
        // cannot spin the loop.
        const MAX_CONSECUTIVE_READ_ERRORS: u32 = 16;
        loop {
            match read_capped_line(&mut reader, &mut raw_line, MAX_AGENT_LINE_BYTES) {
                Ok((0, _)) => break,
                Ok((_, cut)) => {
                    read_errors = 0;
                    if cut {
                        stdout_raw.push_str("[line truncated]\n");
                    }
                }
                // Keep draining after a read error. Stopping here left the CLI
                // blocked on a full pipe until the watchdog killed it, and lost
                // the turn's result. The error is recorded so a truncated
                // stdout log says why.
                Err(e) => {
                    read_errors += 1;
                    stdout_raw.push_str(&format!("[stdout read error: {e}]\n"));
                    if read_errors >= MAX_CONSECUTIVE_READ_ERRORS {
                        break;
                    }
                    continue;
                }
            }
            // Lossy conversion: one invalid byte must not end the stream.
            let text = String::from_utf8_lossy(&raw_line);
            let line = text.trim_end_matches(['\n', '\r']);
            last_output.store(
                started.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::SeqCst,
            );
            // Keep a bounded copy: a long turn streams every Edit/Write input,
            // and the raw copy is only for post-mortem reading.
            if (stdout_raw.len() as u64) < MAX_AGENT_STREAM_BYTES {
                stdout_raw.push_str(line);
                stdout_raw.push('\n');
            } else if !stdout_raw.ends_with("[stdout truncated]\n") {
                stdout_raw.push_str("[stdout truncated]\n");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                unparsed_lines += 1;
                continue;
            };
            match event.get("type").and_then(|t| t.as_str()) {
                Some("assistant") => {
                    if let Some(msg) = event.get("message")
                        && let Some(u) = msg.get("usage")
                    {
                        let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        message_usage.insert(id.to_string(), usage_from_message(u));
                    }
                    // Text and tool_use arrive as content blocks of the
                    // assistant message — there is no top-level `tool_use`
                    // stream event to match on.
                    if let Some(blocks) = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in blocks {
                            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("(unnamed)");
                                let mut input = block
                                    .get("input")
                                    .map(|i| i.to_string())
                                    .unwrap_or_default();
                                // Whole-file Write inputs can be megabytes; the
                                // StructuredOutput fallback needs only a small one.
                                if input.len() > MAX_TOOL_INPUT_BYTES {
                                    input.truncate(input.floor_char_boundary(MAX_TOOL_INPUT_BYTES));
                                }
                                let summary: String = input.chars().take(160).collect();
                                if let Some(cb) = req.on_tool_use {
                                    cb(name, &summary);
                                }
                                tool_calls.push(ToolCall {
                                    name: name.to_string(),
                                    input,
                                });
                            }
                        }
                    }
                }
                Some("result") => {
                    if let Ok(ev) = serde_json::from_value::<ResultEvent>(event) {
                        result_event = ev;
                        saw_result = true;
                    }
                }
                _ => {}
            }
        }
    }

    if !saw_result {
        ap_core::diag!(
            "  warning: the claude stream had no readable result event \
             ({unparsed_lines} line(s) were not JSON); the turn's cost and \
             response are read from what was streamed"
        );
    }

    // The exit is awaited without reaping the CLI, so it still owns its pid
    // while its group is swept; the watchdog and the shutdown sweep are then
    // stood down before the reap frees the number.
    let exited = ap_core::pgroup::wait_exited(child.id());
    // The CLI is gone; reap anything it left in its group, then give the
    // stderr reader a moment (a surviving descendant could hold the pipe).
    crate::proc::kill_process_group(child.id());
    *child_alive.lock().unwrap_or_else(|e| e.into_inner()) = false;
    let _ = done_tx.send(());
    drop(group);
    let status = child.wait().map_err(Error::Io)?;
    exited.map_err(Error::Io)?;
    let mut stderr_content = stderr_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_else(|e| format!("[stderr unavailable: {e}]\n"));
    if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
        stderr_content = format!(
            "claude turn was killed: no output for {} minutes, or over {} minutes in total\n{stderr_content}",
            idle_timeout.as_secs() / 60,
            turn_timeout.as_secs() / 60
        );
    }

    // Lift the structured end-early signal out of the result event once,
    // so both the error branch (no usable response) and the Ok branch
    // (StructuredOutput rescued despite the cut-off) can see it. On the
    // Ok branch the pipeline uses it to emit a warning; otherwise a
    // budget-cut-but-rescued turn would look indistinguishable from a
    // clean one in the event stream and the per-package budget would
    // silently run out one turn later with no visible warning.
    let ended_early = ended_early_signal(&result_event);

    // Prefer the `result` event's payload (the normal success path), but fall
    // back to the last `StructuredOutput` tool call the model emitted during
    // the stream when it's empty or unparseable. This matters most for the
    // budget-exhausted case: the claude CLI exits 1 and leaves
    // `result_event.result` empty even when the model DID produce a valid
    // `StructuredOutput` call before being cut off, and treating that as a
    // hard package failure throws away a usable fix for no good reason.
    // A PatchReview turn that gives up (`blocked`) has no verdict to give.
    let needs_answer = |r: &TurnResponse| match req.task {
        Task::RefreshPatches => !r.patches.is_empty(),
        Task::PatchReview => r.review.is_some() || r.action == "blocked",
        _ => true,
    };
    let resp = match pick_turn_response_where(&result_event.result, &tool_calls, needs_answer) {
        Some(r) => r,
        None => {
            // Prefer the structured end-early signals the claude CLI
            // emits on its final `result` event (`is_error: true` plus
            // `subtype: "error_max_budget_usd"` / `"error_rate_limited"`
            // / etc.) over the opaque "process exited N". The user's
            // drogon-build crash was exactly this: the model made real
            // progress for 10+ turns, hit the --max-budget-usd flag's
            // $0.03 remaining, and the pipeline reported
            // "claude exited 1" with empty stderr — nothing in that
            // output told the user to raise --budget-usd.
            if let Some(signal) = ended_early {
                return Err(Error::EndedEarly(Box::new(EndedEarlyError {
                    subtype: signal.subtype,
                    message: signal.message,
                    stdout: stdout_raw,
                    prompt: prompt.clone(),
                    turns: result_event.num_turns,
                    usage: Usage {
                        input_tokens: result_event.usage.input_tokens,
                        output_tokens: result_event.usage.output_tokens,
                        cli_cost_usd: result_event.total_cost_usd,
                        ..Usage::default()
                    },
                })));
            }
            let usage = turn_usage(&result_event, &message_usage, remaining_usd);
            return Err(if !status.success() {
                Error::Failed {
                    code: status.code().unwrap_or(-1),
                    stderr: stderr_content,
                    stdout: stdout_raw,
                    prompt: prompt.clone(),
                    usage,
                }
            } else {
                // A RefreshPatches answer that names no patch, or a PatchReview
                // answer without a verdict, is a retryable "no usable turn
                // response", the same as a missing answer.
                let detail = if pick_turn_response(&result_event.result, &tool_calls).is_some() {
                    if req.task == Task::PatchReview {
                        "the answer has no `review`, which a PatchReview turn must give"
                    } else {
                        "the answer has no `patches` list, which a RefreshPatches turn must give"
                    }
                    .to_string()
                } else {
                    format!(
                        "result event is {:?} and no StructuredOutput tool call was made",
                        result_event.result
                    )
                };
                Error::Parse {
                    message: format!("no usable turn response: {detail}"),
                    stdout: stdout_raw,
                    prompt: prompt.clone(),
                    usage,
                }
            });
        }
    };

    let usage = Usage {
        input_tokens: result_event.usage.input_tokens,
        output_tokens: result_event.usage.output_tokens,
        cli_cost_usd: result_event.total_cost_usd,
        ..Usage::default()
    };

    let action = match resp.action.as_str() {
        "edited" => Action::Edited,
        "no_change" => Action::NoChange,
        "use_upstream_wheel" => Action::UseUpstreamWheel { why: resp.why },
        "blocked" => Action::Blocked { why: resp.why },
        other => {
            return Err(Error::Parse {
                message: format!("unknown action {other:?} in the turn response"),
                stdout: stdout_raw,
                prompt: prompt.clone(),
                usage: Usage {
                    input_tokens: result_event.usage.input_tokens,
                    output_tokens: result_event.usage.output_tokens,
                    cli_cost_usd: result_event.total_cost_usd,
                    ..Usage::default()
                },
            });
        }
    };

    // Build minimal output summary for output.md
    let output = if tool_calls.is_empty() {
        format!(
            "No tool calls made.\nModel turns: {}\nAction: {:?}\n",
            result_event.num_turns, action
        )
    } else {
        let tool_summary = tool_calls
            .iter()
            .map(|tc| format!("  • {}", tc.name))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Made {} tool call(s) in {} model turn(s):\n{}\n\nAction: {:?}\n",
            tool_calls.len(),
            result_event.num_turns,
            tool_summary,
            action
        )
    };

    Ok(TurnOutcome {
        action,
        reasoning: resp.reasoning,
        changelog: resp.changelog,
        evidence: resp.evidence,
        unresolvable_packages: resp.unresolvable_packages,
        patch_dependencies: resp.patch_dependencies,
        stage_files: resp.stage_files,
        patch_answers: resp.patches,
        patch_review: resp.review,
        confidence: resp.confidence,
        usage,
        tool_calls,
        prompt,
        output,
        denied_commands: denied_bash_commands(&result_event.permission_denials),
        raw: result_event.result,
        session_id: result_event.session_id,
        ended_early,
    })
}
