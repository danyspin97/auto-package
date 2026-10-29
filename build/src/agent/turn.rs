//! The turn runner's settings and helpers: the test overrides, the program it
//! launches, its deadlines and stream limits, the capped line reader, the model
//! for each attempt, and the entry point with its logged-in check.

use super::run::*;
use super::*;

/// What a test can change about how a turn launches and watches `claude`:
/// the program (a stub script) and the deadlines. Per thread, so parallel
/// tests do not see each other's; the turn reads it before it spawns anything.
#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct TurnTestOverrides {
    pub(super) program: Option<std::path::PathBuf>,
    pub(super) turn_timeout: Option<std::time::Duration>,
    pub(super) idle_timeout: Option<std::time::Duration>,
    pub(super) tick: Option<std::time::Duration>,
}

#[cfg(test)]
thread_local! {
    pub(super) static TURN_TEST_OVERRIDES: std::cell::RefCell<TurnTestOverrides> =
        std::cell::RefCell::new(TurnTestOverrides::default());
}

/// The program that runs a turn: `claude`.
pub(super) fn claude_program() -> std::ffi::OsString {
    #[cfg(test)]
    if let Some(p) = TURN_TEST_OVERRIDES.with(|o| o.borrow().program.clone()) {
        return p.into_os_string();
    }
    "claude".into()
}

/// `(wall-clock limit, idle limit, watchdog tick)` of a turn.
#[cfg(not(test))]
pub(super) fn turn_deadlines() -> (
    std::time::Duration,
    std::time::Duration,
    std::time::Duration,
) {
    (AGENT_TURN_TIMEOUT, AGENT_IDLE_TIMEOUT, AGENT_WATCHDOG_TICK)
}

#[cfg(test)]
pub(super) fn turn_deadlines() -> (
    std::time::Duration,
    std::time::Duration,
    std::time::Duration,
) {
    let o = TURN_TEST_OVERRIDES.with(|o| o.borrow().clone());
    (
        o.turn_timeout.unwrap_or(AGENT_TURN_TIMEOUT),
        o.idle_timeout.unwrap_or(AGENT_IDLE_TIMEOUT),
        o.tick.unwrap_or(AGENT_WATCHDOG_TICK),
    )
}

/// Wall-clock limit for one `claude` turn.
const AGENT_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// A turn that prints nothing for this long is stalled (a dead connection, a
/// tool waiting on input), so it is killed well before the wall-clock limit.
const AGENT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How often the watchdog checks for idleness.
const AGENT_WATCHDOG_TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// Most stdout/stderr text kept from one turn for post-mortem files.
pub(super) const MAX_AGENT_STREAM_BYTES: u64 = 32 * 1024 * 1024;

/// Longest stdout line kept in memory. A line past it is cut, and the rest of
/// it is read and dropped up to the newline, so one newline-less line cannot
/// grow the buffer without bound. Far above any real stream-json event.
pub(super) const MAX_AGENT_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Read one line into `line`, keeping at most `cap` bytes of it. The bytes past
/// the cap are consumed and discarded up to and including the newline, so the
/// next call starts on the following line. Returns the number of bytes consumed
/// (0 at end of input) and whether the line was cut.
pub(super) fn read_capped_line<R: std::io::BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<(usize, bool)> {
    line.clear();
    let mut consumed = 0usize;
    let mut cut = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (done, used) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (true, i + 1),
            None => (false, buf.len()),
        };
        let room = cap - line.len();
        let keep = used.min(room);
        line.extend_from_slice(&buf[..keep]);
        if keep < used {
            cut = true;
        }
        consumed += used;
        reader.consume(used);
        if done {
            break;
        }
    }
    Ok((consumed, cut))
}

/// Most bytes kept of one tool call's input.
pub(super) const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;

/// Models the agent runs on, cheapest first. Each has its own API quota.
const AGENT_MODELS: [&str; 3] = ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"];

/// The model for an attempt: escalate with the attempt number (the first 2
/// attempts on Haiku 5.5, the next 3 on Sonnet 5.5, from attempt 6 on — the
/// packages the cheaper tiers already failed — on Opus 5.5), then move
/// `rotation` places along the list (wrapping) to dodge a model whose quota is
/// exhausted.
pub(crate) fn model_for_attempt(attempt: usize, rotation: usize) -> &'static str {
    let base = if attempt <= 2 {
        0
    } else if attempt <= 5 {
        1
    } else {
        2
    };
    AGENT_MODELS[(base + rotation) % AGENT_MODELS.len()]
}

/// Run one agent turn. The agent has tool access to the workspace and edits
/// `spec` in place. The caller is responsible for validating and rolling back
/// on failure.
///
/// Uses `--output-format stream-json` so the agent's text and tool calls are
/// printed to stderr in real time as they arrive. The structured JSON response
/// is parsed from the final `result` event in the stream.
pub fn turn(req: TurnRequest<'_>) -> Result<TurnOutcome, Error> {
    let result = turn_inner(req);
    // Every caller (build, notes, discovery, translate-deps) goes through
    // here: a logged-out CLI cannot do any work, so stop the whole run rather
    // than let each package retry and escalate into the same wall.
    if let Err(e) = &result
        && e.is_not_logged_in()
    {
        crate::proc::abort_run(NOT_LOGGED_IN_MESSAGE.to_string());
    }
    result
}

/// What the user is told when the claude CLI is not logged in.
pub const NOT_LOGGED_IN_MESSAGE: &str =
    "claude is not logged in \u{2014} run `claude` and `/login`, then re-run";

/// Whether text the claude CLI itself produced (a failed `result` event's
/// error, its stderr) says it is not authenticated. Case-insensitive.
///
/// Only feed this claude's own error fields, never the agent's prose or a
/// build log: the phrases are specific enough to avoid a bare "login", but
/// prose can still quote them.
pub fn looks_like_not_logged_in(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "not logged in",
        "please run /login",
        "run /login",
        "invalid api key",
        "authentication_error",
        "api_error_401",
        "api error: 401",
        "failed to authenticate",
        "oauth token has expired",
        "token expired",
    ];
    let lower = text.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Whether `claude auth status` output says the CLI is logged out: JSON with
/// `"loggedIn": false`. Anything else (missing field, odd output) is not
/// treated as logged out, so an unfamiliar CLI version never blocks a run.
pub(super) fn status_says_logged_out(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool))
        == Some(false)
}

/// Fail fast, before any package work, when the claude CLI is logged out.
/// `claude auth status` makes no model call, so this costs nothing. A CLI
/// that cannot be run or answers unexpectedly passes: only a clear
/// `loggedIn: false` is an error.
pub fn ensure_logged_in() -> Result<(), String> {
    let env = agent_env();
    let out = crate::proc::capture_with_timeout_env(
        "claude",
        &["auth", "status"],
        std::path::Path::new("."),
        crate::proc::Emit::quiet(),
        std::time::Duration::from_secs(30),
        Some(&env),
    );
    match out {
        Ok(c) if status_says_logged_out(&c.stdout) => Err(NOT_LOGGED_IN_MESSAGE.to_string()),
        _ => Ok(()),
    }
}
