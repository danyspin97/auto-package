//! The OBS API and osc calls: the timeouts, the query and the checked API call.

use super::*;

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
pub(super) const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for `osc co`, `up` and `ci`, which move a package's sources.
pub(super) const OSC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Bound for `osc buildlog` and `getbinaries`, which move whole logs and RPM
/// sets and so run longer than a source transfer.
pub(super) const OSC_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Tries made for a read-only `osc` query that OBS answers with a 502, 503 or
/// 504: the first try and two repeats.
pub(super) const QUERY_ATTEMPTS: u32 = 3;

/// Whether an `osc` answer is OBS saying it is briefly unavailable: a bad
/// gateway, an unavailable or a timed-out server.
fn is_gateway_failure(out: &Captured) -> bool {
    !out.success() && matches!(http_status(&out.stderr), Some(502..=504))
}

/// The pause before try `attempt + 1` of a query (`attempt` counts from 1): 2s,
/// then 4s.
fn gateway_backoff(attempt: u32) -> Duration {
    Duration::from_secs(2 * u64::from(attempt))
}

/// Run `run` (one try of an `osc` query), repeating it while OBS answers 502,
/// 503 or 504, up to [`QUERY_ATTEMPTS`] tries and not once the run is shutting
/// down. `pause` waits before the next try. Any other answer, and a launch
/// failure or timeout, is returned as it is.
pub(super) fn retry_gateway_failures<E>(
    mut run: impl FnMut() -> Result<Captured, E>,
    mut pause: impl FnMut(Duration),
) -> Result<Captured, E> {
    let mut attempt = 1;
    loop {
        let out = run();
        let again = matches!(&out, Ok(o) if is_gateway_failure(o));
        if !again || attempt >= QUERY_ATTEMPTS || crate::proc::shutting_down() {
            return out;
        }
        pause(gateway_backoff(attempt));
        attempt += 1;
    }
}

/// Run a read-only `osc` command with a timeout, repeating a gateway failure
/// (see [`retry_gateway_failures`]). The answer is what the last try gave.
pub(super) fn capture_query(
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &std::path::Path,
    emit: Emit<'_>,
    timeout: Duration,
) -> Result<Captured, proc::ProcessError> {
    retry_gateway_failures(
        || proc::capture_with_timeout("osc", args, cwd, emit, timeout),
        super::poll::sleep_interruptible,
    )
}

/// Whether an `osc api` call is a read that may be repeated over a gateway
/// failure: a plain `GET`. A call with a method (`-X POST`, `-X DELETE`) or a
/// body (`-d`) writes, and a write is never repeated.
pub(super) fn api_is_retried(method: Option<&str>, data: Option<&str>) -> bool {
    method.is_none() && data.is_none()
}

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
pub(super) fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    capture_query(args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
        .map_err(|e| ap_core::diag!("  osc {what}: {e}"))
        .ok()
}

/// Why an `osc api` read gave no document.
#[derive(Clone)]
pub(super) enum ApiFailure {
    /// The server answered with this HTTP status (401, 429, 503, ...): asking
    /// again through another `osc` command will not get a different answer.
    Http(u16, String),
    /// `osc` could not run, timed out, or failed without an HTTP status.
    Other(String),
}

/// The HTTP status in an `osc` error message, in either wording `osc`
/// versions use (`HTTP Error 503: ...`, `503 Server Error: ...`).
pub fn http_status(stderr: &str) -> Option<u16> {
    let digits = |s: &str| {
        let n: String = s.chars().take_while(char::is_ascii_digit).collect();
        (n.len() == 3).then(|| n.parse().ok()).flatten()
    };
    if let Some((_, rest)) = stderr.split_once("HTTP Error ") {
        return digits(rest);
    }
    stderr
        .split_whitespace()
        .zip(stderr.split_whitespace().skip(1))
        .find_map(|(code, kind)| {
            matches!(kind, "Client" | "Server")
                .then(|| digits(code))
                .flatten()
        })
}

/// The `osc` arguments of an `api` read, against `apiurl` when given.
pub(super) fn api_args<'a>(apiurl: Option<&'a str>, path: &'a str) -> Vec<&'a str> {
    let mut args: Vec<&str> = Vec::new();
    if let Some(url) = apiurl {
        args.extend(["-A", url]);
    }
    args.extend(["api", path]);
    args
}

/// `osc api <path>` stdout, or why there is none.
pub(super) fn osc_api_checked(path: &str) -> Result<String, ApiFailure> {
    osc_api_checked_at(None, path)
}

/// [`osc_api_checked`] against the API server `apiurl` (`osc -A`) instead of
/// osc's default one.
pub(super) fn osc_api_checked_at(apiurl: Option<&str>, path: &str) -> Result<String, ApiFailure> {
    let args = api_args(apiurl, path);
    let out = capture_query(&args, &anywhere(), Emit::quiet(), OSC_QUERY_TIMEOUT)
        .map_err(|e| ApiFailure::Other(format!("osc api {path}: {e}")))?;
    if !out.success() {
        let msg = format!("osc api {path}: exit {}: {}", out.code, out.stderr.trim());
        return Err(match http_status(&out.stderr) {
            Some(code) => ApiFailure::Http(code, msg),
            None => ApiFailure::Other(msg),
        });
    }
    Ok(out.stdout)
}

/// `osc api <path>` stdout, or `None` on any failure or timeout.
pub(super) fn osc_api(path: &str) -> Option<String> {
    osc_api_checked(path)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
pub(super) fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}
