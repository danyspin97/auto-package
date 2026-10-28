use super::*;

/// Set when the process is told to stop; retry pauses end early then.
pub(super) static STOP: AtomicBool = AtomicBool::new(false);

/// Tell retry loops to give up instead of waiting out their pauses.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// Sleep for `wait`, waking early when `stop` is set. Returns whether it was.
pub(super) fn sleep_unless_stopped(wait: Duration, stop: &AtomicBool) -> bool {
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
    pub(super) fn acquire(&self, host: &str) -> bool {
        let wait = self.reserve(host, Instant::now());
        !wait.is_zero() && sleep_unless_stopped(wait, &STOP)
    }
}

/// The one limiter every request through [`call_with_retries_detailed`] shares.
pub(super) fn host_limiter() -> &'static HostLimiter {
    static LIMITER: OnceLock<HostLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| {
        HostLimiter::new(
            ap_core::env::env_positive_f64("AUTO_PACKAGE_HOST_RPS").unwrap_or(HOST_RATE),
            ap_core::env::env_positive_f64("AUTO_PACKAGE_HOST_BURST").unwrap_or(HOST_BURST),
        )
    })
}

pub(super) fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
