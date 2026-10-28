use super::*;

/// Requests made for one call: the first try plus two retries.
pub const ATTEMPTS: u32 = 3;
/// Longest pause taken between attempts for a `Retry-After` or a rate-limit
/// reset the server announced.
pub(super) const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
/// Longest backoff before jitter, when the server named no time.
pub(super) const MAX_BACKOFF: Duration = Duration::from_secs(32);

/// Whether a failed request is worth repeating: a dropped connection, a
/// timeout or a failed lookup. A refused private target, a bad URL, an
/// oversized body or a TLS fault would fail the same way again.
pub(super) fn is_transient(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Io(e) => e.kind() != std::io::ErrorKind::PermissionDenied,
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        _ => false,
    }
}

/// The exponential backoff before attempt `attempt + 1` (`attempt` counts from
/// 1): 1s, 2s, 4s ... up to [`MAX_BACKOFF`]. The pause actually taken is a
/// uniform draw from zero to this (full jitter), so threads that failed
/// together do not retry together.
pub(super) fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << (attempt.max(1) - 1).min(5)).min(MAX_BACKOFF)
}

/// The rate-limit related headers of an error answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyHeaders {
    /// `Retry-After` in seconds (an HTTP-date is not understood).
    pub retry_after: Option<u64>,
    /// `x-ratelimit-remaining`.
    pub remaining: Option<u64>,
    /// `x-ratelimit-reset`, seconds since the epoch.
    pub reset: Option<u64>,
}

impl ReplyHeaders {
    fn from_response(resp: &ureq::http::Response<ureq::Body>) -> Self {
        let num = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        ReplyHeaders {
            retry_after: num("retry-after"),
            remaining: num("x-ratelimit-remaining"),
            reset: num("x-ratelimit-reset"),
        }
    }
}

/// An error answer that ended a call, with what the server said about rate
/// limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFailure {
    pub status: u16,
    pub headers: ReplyHeaders,
}

impl StatusFailure {
    /// A 429, or a 403 whose quota is used up (`x-ratelimit-remaining: 0`).
    pub fn is_rate_limit(&self) -> bool {
        self.status == 429 || (self.status == 403 && self.headers.remaining == Some(0))
    }
}

/// Why [`call_with_retries_detailed`] gave up.
#[derive(Debug)]
pub enum CallFailure {
    Status(StatusFailure),
    Transport(ureq::Error),
}

impl From<CallFailure> for ureq::Error {
    fn from(f: CallFailure) -> Self {
        match f {
            CallFailure::Status(s) => ureq::Error::StatusCode(s.status),
            CallFailure::Transport(e) => e,
        }
    }
}

/// What to do about an error answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    Final,
    /// Wait exactly this long (the server named the time).
    Wait(Duration),
    /// Wait a jittered backoff of at most this long.
    Backoff(Duration),
}

/// Decide what an error answer means. `now` is seconds since the epoch.
///
/// 429 and 5xx are repeated. A 403 is final unless its quota is used up
/// (`x-ratelimit-remaining: 0`), in which case it is repeated after the
/// announced reset when that is within [`MAX_RETRY_WAIT`]; a reset further off
/// (GitHub's unauthenticated hour) is final.
pub(super) fn judge(status: u16, h: &ReplyHeaders, attempt: u32, now: u64) -> Verdict {
    if attempt >= ATTEMPTS {
        return Verdict::Final;
    }
    let announced = h.retry_after.map(Duration::from_secs).or_else(|| {
        h.reset
            .map(|r| Duration::from_secs(r.saturating_sub(now).saturating_add(1)))
    });
    let bounded = |d: Duration| {
        if d > MAX_RETRY_WAIT {
            Verdict::Final
        } else {
            Verdict::Wait(d)
        }
    };
    match status {
        429 => match h.retry_after.map(Duration::from_secs) {
            // Retry-After beyond the bound is clamped, as before; a reset
            // that far off is not worth waiting for.
            Some(d) => Verdict::Wait(d.min(MAX_RETRY_WAIT)),
            None => match announced {
                Some(d) => bounded(d),
                None => Verdict::Backoff(backoff(attempt)),
            },
        },
        403 if h.remaining == Some(0) => match announced {
            Some(d) => bounded(d),
            None => Verdict::Final,
        },
        s if s >= 500 => match h.retry_after.map(Duration::from_secs) {
            Some(d) => Verdict::Wait(d.min(MAX_RETRY_WAIT)),
            None => Verdict::Backoff(backoff(attempt)),
        },
        _ => Verdict::Final,
    }
}

/// One try, as the retry loop sees it.
pub(super) enum Outcome<T> {
    Done(T),
    Status(u16, ReplyHeaders),
    Error(ureq::Error),
}

/// A uniform draw from `[0, max]`, from a small per-process generator: the
/// quality only has to spread threads apart.
pub(super) fn jitter(max: Duration) -> Duration {
    use std::sync::atomic::AtomicU64;
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    let millis = max.as_millis() as u64;
    Duration::from_millis(if millis == 0 { 0 } else { x % (millis + 1) })
}

/// The retry loop with its clock and sleep injected. `sleep` returns true when
/// the wait was cut short by a stop request.
pub(super) fn retry_loop<T>(
    mut once: impl FnMut() -> Outcome<T>,
    now: impl Fn() -> u64,
    mut jittered: impl FnMut(Duration) -> Duration,
    mut sleep: impl FnMut(Duration) -> bool,
) -> Result<T, CallFailure> {
    let mut attempt = 1;
    loop {
        let wait = match once() {
            Outcome::Done(v) => return Ok(v),
            Outcome::Status(status, headers) => match judge(status, &headers, attempt, now()) {
                Verdict::Final => {
                    return Err(CallFailure::Status(StatusFailure { status, headers }));
                }
                Verdict::Wait(d) => d,
                Verdict::Backoff(max) => jittered(max),
            },
            Outcome::Error(e) if is_transient(&e) && attempt < ATTEMPTS => {
                jittered(backoff(attempt))
            }
            Outcome::Error(e) => return Err(CallFailure::Transport(e)),
        };
        if sleep(wait) {
            return Err(CallFailure::Transport(ureq::Error::Io(
                std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "interrupted while waiting to retry",
                ),
            )));
        }
        attempt += 1;
    }
}

/// Send the request `build` makes, up to [`ATTEMPTS`] times. Transport
/// errors, 429 and 5xx answers, and a 403 that says the quota is used up (when
/// it resets soon) are repeated after a pause. Any other answer is final. A
/// final 4xx/5xx comes back as `ureq::Error::StatusCode`, as it does without
/// this wrapper. Only for requests that are safe to repeat.
pub fn call_with_retries(
    build: impl FnMut() -> ureq::RequestBuilder<ureq::typestate::WithoutBody>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    call_with_retries_detailed(build).map_err(Into::into)
}

/// [`call_with_retries`] that keeps what a final error answer said about rate
/// limits. Every attempt first takes a slot from the shared per-host limiter.
pub fn call_with_retries_detailed(
    mut build: impl FnMut() -> ureq::RequestBuilder<ureq::typestate::WithoutBody>,
) -> Result<ureq::http::Response<ureq::Body>, CallFailure> {
    retry_loop(
        || {
            let request = build();
            let host = request
                .uri_ref()
                .and_then(|u| u.host())
                .map(str::to_ascii_lowercase);
            if let Some(host) = host
                && host_limiter().acquire(&host)
            {
                return Outcome::Error(ureq::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "interrupted while waiting for a request slot",
                )));
            }
            // Status errors are switched off so the headers of a 429 stay readable.
            match request.config().http_status_as_error(false).build().call() {
                Ok(resp) if resp.status().as_u16() < 400 => Outcome::Done(resp),
                Ok(resp) => {
                    Outcome::Status(resp.status().as_u16(), ReplyHeaders::from_response(&resp))
                }
                Err(e) => Outcome::Error(e),
            }
        },
        epoch_now,
        jitter,
        |wait| sleep_unless_stopped(wait, &STOP),
    )
}
