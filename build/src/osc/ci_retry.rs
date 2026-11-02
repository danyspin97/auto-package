//! Retrying an `osc ci` that hit a transient server error: a 5xx, a timeout or
//! a dropped connection. A refusal (a 4xx) is the answer and is not retried.

use super::*;
use crate::osc::query::*;
use std::path::Path;

/// The waits before the second and the third try of a commit that hit a
/// transient error: three tries in all.
pub(super) const CI_RETRY_WAITS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(15)];

/// Whether a failed commit is worth another try. A 4xx other than 408 and 429
/// is OBS saying no, whatever words the text carries (a 404 that names a
/// timeout included).
pub(super) fn ci_error_is_transient(err: &Error) -> bool {
    if let Error::OscFailed { stderr, .. } = err
        && let Some(code) = http_status(stderr)
        && (400..500).contains(&code)
        && code != 408
        && code != 429
    {
        return false;
    }
    err.is_transient()
}

/// Run `attempt` until it succeeds or fails for good, waiting `CI_RETRY_WAITS`
/// between tries that failed transiently. `wait` does the waiting.
pub(super) fn with_ci_retries(
    mut attempt: impl FnMut() -> Result<(), Error>,
    mut wait: impl FnMut(Duration),
) -> Result<(), Error> {
    let mut waits = CI_RETRY_WAITS.iter();
    loop {
        match attempt() {
            Err(e) if ci_error_is_transient(&e) => match waits.next() {
                Some(delay) => {
                    ap_core::diag!(
                        "  osc ci hit a transient error, trying again in {}s: {e}",
                        delay.as_secs()
                    );
                    wait(*delay);
                }
                None => return Err(e),
            },
            other => return other,
        }
    }
}

/// One `osc ci` of `ci` (its arguments) in `checkout_dir`. A failure has its
/// full output written to `log` when given; "nothing to do" is a success.
pub(super) fn ci_once(
    checkout_dir: &Path,
    message: &str,
    ci: &[String],
    log: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let out = proc::capture_with_timeout("osc", ci, checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
        .map_err(during(format!("ci in {}", checkout_dir.display())))?;
    if out.success() {
        return Ok(());
    }
    // "nothing to do" means files are already committed — not an error.
    if out.stdout.contains("nothing to do") || out.stderr.contains("nothing to do") {
        return Ok(());
    }
    let mut detail = ci_failure_detail(&out.stdout, &out.stderr);
    if let Some(log) = log {
        let full = format!("--- stdout\n{}\n--- stderr\n{}\n", out.stdout, out.stderr);
        match ap_core::fsx::write_atomic(log, full) {
            Ok(()) => detail.push_str(&format!("\nthe full output is in {}", log.display())),
            Err(e) => detail.push_str(&format!("\nthe full output cannot be saved: {e}")),
        }
    }
    Err(Error::OscFailed {
        cmd: format!("ci -m {message:?} in {}", checkout_dir.display()),
        code: out.code,
        stderr: detail,
    })
}
