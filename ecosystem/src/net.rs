//! Outbound HTTP agents that only reach public hosts over https.
//!
//! The URLs fetched come from upstream metadata, so a redirect or a DNS answer
//! could point at the loopback interface, the local network or a cloud
//! metadata endpoint. The check sits in the resolver, which ureq consults for
//! every connection including each redirect hop, and it applies to the address
//! actually connected to, so a name that resolves differently later is covered.
//!
//! Behind a proxy the proxy resolves the target, so only literal IP targets
//! can be refused (see [`LiteralIpGuard`]); a name that resolves to a private
//! address is the proxy's to deal with.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

/// Addresses a fetch must never connect to: loopback, private, link-local,
/// carrier-grade NAT, multicast and unspecified ones.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 100.64.0.0/10, carrier-grade NAT
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // 0.0.0.0/8
        || o[0] == 0
        // 240.0.0.0/4, reserved
        || o[0] >= 240
        // 198.18.0.0/15, benchmarking
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // 192.0.0.0/24, IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let seg = ip.segments();
    // The v4 address carried in the low 32 bits of the translation forms.
    let low = Ipv4Addr::new(
        (seg[6] >> 8) as u8,
        seg[6] as u8,
        (seg[7] >> 8) as u8,
        seg[7] as u8,
    );
    // 64:ff9b::/96, NAT64
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return forbidden_v4(low);
    }
    // 2002::/16, 6to4: the v4 address is in segments 1 and 2
    if seg[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        );
        return forbidden_v4(embedded);
    }
    // ::a.b.c.d, the deprecated IPv4-compatible form (::1 is loopback, below)
    if seg[..6] == [0; 6] && !ip.is_loopback() && !ip.is_unspecified() {
        return forbidden_v4(low);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fe80::/10 link-local, fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xfe00) == 0xfc00
}

/// Whether `host` (as in a URI, IPv6 in brackets) is an IP literal that is
/// forbidden, or a numeric form (`2130706433`, `0x7f.1`) that std does not
/// parse but a proxy's resolver might read as an address.
fn is_forbidden_literal(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_forbidden_ip(ip);
    }
    // No real name ends in a numeric label.
    let last = bare.rsplit('.').next().unwrap_or("");
    !last.is_empty()
        && (last.bytes().all(|b| b.is_ascii_digit()) || last.to_ascii_lowercase().starts_with("0x"))
}

/// Refuses requests whose target is a forbidden IP literal when a proxy is
/// configured. The proxy resolves the target, so [`PublicOnlyResolver`] never
/// sees it; without a proxy the resolver covers literals too.
#[derive(Debug)]
struct LiteralIpGuard;

impl<In: Transport> Connector<In> for LiteralIpGuard {
    type Out = In;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<In>, ureq::Error> {
        if details.config.proxy().is_some()
            && let Some(host) = details.uri.host()
            && is_forbidden_literal(host)
        {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: private or local address", details.uri),
            )));
        }
        Ok(chained)
    }
}

#[derive(Debug, Default)]
struct PublicOnlyResolver {
    inner: DefaultResolver,
}

impl Resolver for PublicOnlyResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.inner.resolve(uri, config, timeout)?;
        // Through a proxy the address resolved here is the proxy's, which an
        // operator chose and which may well be on the local network.
        if config.proxy().is_some() {
            return Ok(resolved);
        }
        let mut allowed = self.empty();
        for addr in resolved.iter().filter(|a| !is_forbidden_ip(a.ip())) {
            allowed.push(*addr);
        }
        if allowed.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{uri}: resolves only to private or local addresses"),
            )));
        }
        Ok(allowed)
    }
}

/// An agent for `config` that speaks https only (redirects included) and
/// refuses private and local addresses. `config` should come from
/// `ureq::Agent::config_builder()` so proxy settings are read from the
/// environment.
pub fn agent(config: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    ureq::Agent::with_parts(
        config.https_only(true).build(),
        LiteralIpGuard.chain(DefaultConnector::default()),
        PublicOnlyResolver::default(),
    )
}

/// Requests made for one call: the first try plus two retries.
pub const ATTEMPTS: u32 = 3;
/// Longest pause taken between attempts for a `Retry-After` or a rate-limit
/// reset the server announced.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);
/// Longest backoff before jitter, when the server named no time.
const MAX_BACKOFF: Duration = Duration::from_secs(32);

/// Whether a failed request is worth repeating: a dropped connection, a
/// timeout or a failed lookup. A refused private target, a bad URL, an
/// oversized body or a TLS fault would fail the same way again.
fn is_transient(err: &ureq::Error) -> bool {
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
fn backoff(attempt: u32) -> Duration {
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
enum Verdict {
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
fn judge(status: u16, h: &ReplyHeaders, attempt: u32, now: u64) -> Verdict {
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
enum Outcome<T> {
    Done(T),
    Status(u16, ReplyHeaders),
    Error(ureq::Error),
}

/// A uniform draw from `[0, max]`, from a small per-process generator: the
/// quality only has to spread threads apart.
fn jitter(max: Duration) -> Duration {
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
fn retry_loop<T>(
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

/// Set when the process is told to stop; retry pauses end early then.
static STOP: AtomicBool = AtomicBool::new(false);

/// Tell retry loops to give up instead of waiting out their pauses.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// Sleep for `wait`, waking early when `stop` is set. Returns whether it was.
fn sleep_unless_stopped(wait: Duration, stop: &AtomicBool) -> bool {
    const SLICE: Duration = Duration::from_millis(100);
    let end = Instant::now() + wait;
    loop {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(left.min(SLICE));
    }
}

/// A token bucket per host: `rate` requests a second with room for `burst`.
/// The clock is passed in, so tests need no sleeping.
#[derive(Debug)]
pub struct HostLimiter {
    rate: f64,
    burst: f64,
    hosts: Mutex<HashMap<String, (f64, Instant)>>,
}

/// Default requests per second per host, and the burst allowed.
pub const HOST_RATE: f64 = 4.0;
pub const HOST_BURST: f64 = 8.0;

impl HostLimiter {
    pub fn new(rate: f64, burst: f64) -> Self {
        HostLimiter {
            rate: rate.max(0.001),
            burst: burst.max(1.0),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// Take a token for `host` at `now`: zero when one was free, else how long
    /// to wait. The token is spent either way (the balance goes negative), so
    /// callers queued behind each other get successive slots.
    pub fn reserve(&self, host: &str, now: Instant) -> Duration {
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        let (tokens, at) = hosts.entry(host.to_string()).or_insert((self.burst, now));
        let earned = now.saturating_duration_since(*at).as_secs_f64() * self.rate;
        *tokens = (*tokens + earned).min(self.burst);
        *at = now;
        *tokens -= 1.0;
        if *tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-*tokens / self.rate)
        }
    }

    /// Wait for a slot for `host`. Returns true when stopped while waiting.
    fn acquire(&self, host: &str) -> bool {
        let wait = self.reserve(host, Instant::now());
        !wait.is_zero() && sleep_unless_stopped(wait, &STOP)
    }
}

/// The one limiter every request through [`call_with_retries_detailed`] shares.
fn host_limiter() -> &'static HostLimiter {
    static LIMITER: OnceLock<HostLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| {
        HostLimiter::new(
            ap_core::env::env_positive_f64("AUTO_PACKAGE_HOST_RPS").unwrap_or(HOST_RATE),
            ap_core::env::env_positive_f64("AUTO_PACKAGE_HOST_BURST").unwrap_or(HOST_BURST),
        )
    })
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_local_addresses_are_forbidden() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "240.0.0.1",
            "255.255.255.254",
            "198.18.0.1",
            "198.19.255.255",
            "192.0.0.8",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2002:c0a8:101::1",
            "::127.0.0.1",
            "::10.0.0.1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "140.82.112.5",
            "2606:4700::1111",
            "198.20.0.1",
            "192.0.2.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn literal_hosts_are_judged_including_odd_numeric_forms() {
        for host in [
            "127.0.0.1",
            "[::1]",
            "[fd00::1]",
            "2130706433",
            "0x7f.1",
            "10.1",
        ] {
            assert!(is_forbidden_literal(host), "{host}");
        }
        for host in [
            "example.com",
            "1.1.1.1",
            "[2606:4700::1111]",
            "a1.example.org",
        ] {
            assert!(!is_forbidden_literal(host), "{host}");
        }
    }

    #[test]
    fn a_literal_private_target_is_refused_even_behind_a_proxy() {
        let proxy = ureq::Proxy::new("http://127.0.0.1:9").unwrap();
        let a = agent(ureq::Agent::config_builder().proxy(Some(proxy)));
        let err = a.get("https://169.254.169.254/").call().unwrap_err();
        assert!(err.to_string().contains("private or local"), "{err}");
    }

    fn hdr(retry_after: Option<u64>, remaining: Option<u64>, reset: Option<u64>) -> ReplyHeaders {
        ReplyHeaders {
            retry_after,
            remaining,
            reset,
        }
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(30), MAX_BACKOFF);
    }

    #[test]
    fn jitter_stays_within_the_bound() {
        for _ in 0..200 {
            assert!(jitter(Duration::from_secs(4)) <= Duration::from_secs(4));
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn retry_after_is_honoured_up_to_120_seconds() {
        let none = ReplyHeaders::default();
        assert_eq!(
            judge(429, &hdr(Some(90), None, None), 1, 0),
            Verdict::Wait(Duration::from_secs(90))
        );
        assert_eq!(
            judge(503, &hdr(Some(86_400), None, None), 1, 0),
            Verdict::Wait(MAX_RETRY_WAIT)
        );
        assert_eq!(
            judge(500, &none, 2, 0),
            Verdict::Backoff(Duration::from_secs(2))
        );
        assert_eq!(judge(500, &none, ATTEMPTS, 0), Verdict::Final);
        assert_eq!(judge(404, &none, 1, 0), Verdict::Final);
        assert_eq!(judge(403, &none, 1, 0), Verdict::Final);
    }

    #[test]
    fn a_403_with_an_empty_quota_waits_for_a_near_reset_only() {
        // Resets in 10s: wait (plus one second of slack) and try again.
        assert_eq!(
            judge(403, &hdr(None, Some(0), Some(1_010)), 1, 1_000),
            Verdict::Wait(Duration::from_secs(11))
        );
        // GitHub's unauthenticated hour is beyond the bound: final.
        assert_eq!(
            judge(403, &hdr(None, Some(0), Some(4_600)), 1, 1_000),
            Verdict::Final
        );
        // Quota left: an ordinary refusal.
        assert_eq!(
            judge(403, &hdr(None, Some(5), Some(1_010)), 1, 1_000),
            Verdict::Final
        );
        // 429 with a reset but no Retry-After.
        assert_eq!(
            judge(429, &hdr(None, None, Some(1_005)), 1, 1_000),
            Verdict::Wait(Duration::from_secs(6))
        );
    }

    #[test]
    fn rate_limit_detection() {
        let f = |status, h| StatusFailure { status, headers: h };
        assert!(f(429, ReplyHeaders::default()).is_rate_limit());
        assert!(f(403, hdr(None, Some(0), None)).is_rate_limit());
        assert!(!f(403, hdr(None, Some(3), None)).is_rate_limit());
        assert!(!f(500, ReplyHeaders::default()).is_rate_limit());
    }

    #[test]
    fn the_loop_retries_with_the_injected_clock_and_gives_up_after_the_attempts() {
        let mut replies = vec![
            Outcome::Status(429, hdr(Some(7), None, None)),
            Outcome::Status(403, hdr(None, Some(0), Some(1_003))),
            Outcome::Done("ok"),
        ]
        .into_iter();
        let mut slept = Vec::new();
        let got = retry_loop(
            || replies.next().unwrap(),
            || 1_000,
            |max| max,
            |d| {
                slept.push(d);
                false
            },
        );
        assert_eq!(got.unwrap(), "ok");
        assert_eq!(slept, [Duration::from_secs(7), Duration::from_secs(4)]);

        let mut slept = Vec::new();
        let got: Result<(), _> = retry_loop(
            || Outcome::Status(502, ReplyHeaders::default()),
            || 0,
            |max| max / 2,
            |d| {
                slept.push(d);
                false
            },
        );
        assert!(matches!(
            got,
            Err(CallFailure::Status(StatusFailure { status: 502, .. }))
        ));
        // Two pauses, each half (the injected jitter) of the 1s and 2s backoff.
        assert_eq!(slept, [Duration::from_millis(500), Duration::from_secs(1)]);

        // A transient transport error is retried, a refused target is not.
        let mut n = 0;
        let got = retry_loop(
            || {
                n += 1;
                if n == 1 {
                    Outcome::Error(ureq::Error::HostNotFound)
                } else {
                    Outcome::Done(n)
                }
            },
            || 0,
            |m| m,
            |_| false,
        );
        assert_eq!(got.unwrap(), 2);
        let got: Result<(), _> = retry_loop(
            || {
                Outcome::Error(ureq::Error::Io(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )))
            },
            || 0,
            |m| m,
            |_| panic!("must not wait"),
        );
        assert!(matches!(got, Err(CallFailure::Transport(_))));
    }

    #[test]
    fn a_stop_during_the_pause_ends_the_loop() {
        let got: Result<(), _> = retry_loop(
            || Outcome::Status(500, ReplyHeaders::default()),
            || 0,
            |m| m,
            |_| true,
        );
        assert!(matches!(
            got,
            Err(CallFailure::Transport(ureq::Error::Io(e))) if e.kind() == std::io::ErrorKind::Interrupted
        ));
    }

    #[test]
    fn the_host_limiter_allows_a_burst_then_paces_per_host() {
        let l = HostLimiter::new(4.0, 8.0);
        let t0 = Instant::now();
        for _ in 0..8 {
            assert_eq!(l.reserve("a.example", t0), Duration::ZERO);
        }
        // The ninth waits a quarter second, the tenth half a second.
        assert_eq!(l.reserve("a.example", t0), Duration::from_millis(250));
        assert_eq!(l.reserve("a.example", t0), Duration::from_millis(500));
        // Another host has its own bucket.
        assert_eq!(l.reserve("b.example", t0), Duration::ZERO);
        // Time refills it: after 3s the debt of 2 tokens is paid and 8 are spare.
        let later = t0 + Duration::from_secs(3);
        for _ in 0..8 {
            assert_eq!(l.reserve("a.example", later), Duration::ZERO);
        }
        // Never more than the burst, however long it idled.
        let much_later = t0 + Duration::from_secs(3_600);
        for _ in 0..8 {
            assert_eq!(l.reserve("c.example", much_later), Duration::ZERO);
        }
        assert!(l.reserve("c.example", much_later) > Duration::ZERO);
    }

    #[test]
    fn a_retry_pause_ends_early_when_stopped() {
        let stop = AtomicBool::new(true);
        let t = Instant::now();
        assert!(sleep_unless_stopped(Duration::from_secs(30), &stop));
        assert!(t.elapsed() < Duration::from_secs(1));
        let go = AtomicBool::new(false);
        assert!(!sleep_unless_stopped(Duration::from_millis(10), &go));
    }

    #[test]
    fn only_transport_faults_are_transient() {
        assert!(is_transient(&ureq::Error::HostNotFound));
        assert!(!is_transient(&ureq::Error::StatusCode(500)));
        assert!(!is_transient(&ureq::Error::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        ))));
    }

    #[test]
    fn a_loopback_target_is_refused_and_http_is_too() {
        let a = agent(ureq::Agent::config_builder().proxy(None));
        let err = a.get("https://127.0.0.1:1/").call().unwrap_err();
        assert!(err.to_string().contains("private or local"), "{err}");
        assert!(a.get("http://example.com/").call().is_err());
    }
}
