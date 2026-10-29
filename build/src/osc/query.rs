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

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
pub(super) fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    proc::capture_with_timeout("osc", args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
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
pub(super) fn http_status(stderr: &str) -> Option<u16> {
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

/// `osc api <path>` stdout, or why there is none.
pub(super) fn osc_api_checked(path: &str) -> Result<String, ApiFailure> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
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
