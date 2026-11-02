//! Run-wide limits shared by every thread of the process: the daily spend cap
//! and the OBS request rate.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default for `--max-total-usd`: the most one UTC day may spend on agent turns.
pub const DEFAULT_MAX_TOTAL_USD: f64 = 50.0;
/// Default for `--max-packages`: packages one run may start.
pub const DEFAULT_MAX_PACKAGES: usize = 25;
/// Default for `--jobs`.
pub const DEFAULT_JOBS: usize = 4;
/// Default for the fix-after-build rounds a package gets: one fix turn per failed build.
pub const DEFAULT_FIX_BUILD_ROUNDS: usize = 6;
/// Failed builds in a row with the same error lines before the package ends.
pub const SAME_BUILD_ERRORS_LIMIT: usize = 3;
/// Default OBS requests per second (`AUTO_PACKAGE_OBS_RPS` overrides it).
pub const DEFAULT_OBS_RPS: f64 = 5.0;
/// OBS requests allowed back to back before the rate applies.
pub const OBS_BURST: f64 = 10.0;
/// What one agent turn is assumed to cost when checking the daily cap before
/// it starts. The real cost is only known afterwards.
pub const ESTIMATED_TURN_USD: f64 = 0.50;

static MAX_TOTAL_USD_BITS: AtomicU64 = AtomicU64::new(0);

/// Set the daily cap for this process (from `--max-total-usd`).
pub fn set_max_total_usd(usd: f64) {
    MAX_TOTAL_USD_BITS.store(usd.to_bits(), Ordering::SeqCst);
}

/// The daily cap: the value set by the CLI, else [`DEFAULT_MAX_TOTAL_USD`].
pub fn max_total_usd() -> f64 {
    match MAX_TOTAL_USD_BITS.load(Ordering::SeqCst) {
        0 => DEFAULT_MAX_TOTAL_USD,
        bits => f64::from_bits(bits),
    }
}

static LEDGER_DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Set the directory holding `ledger.json`: the work directory root, shared by
/// every project and run that uses it.
pub fn set_ledger_dir(dir: std::path::PathBuf) {
    *LEDGER_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

/// The directory set by [`set_ledger_dir`], if any.
pub fn ledger_dir() -> Option<std::path::PathBuf> {
    LEDGER_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static STARTED: Mutex<Option<std::collections::BTreeSet<String>>> = Mutex::new(None);

/// Count `name` as started by this process. True when it may start: it was
/// started before (a retry), or fewer than `max` distinct packages have
/// started. `max == 0` means no limit.
pub fn try_start_package(name: &str, max: usize) -> bool {
    let mut g = STARTED.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(Default::default);
    if set.contains(name) {
        return true;
    }
    if max != 0 && set.len() >= max {
        return false;
    }
    set.insert(name.to_string());
    true
}

/// A token bucket: `burst` tokens at most, refilled at `rate` per second.
pub struct TokenBucket {
    rate: f64,
    burst: f64,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    pub fn new(rate: f64, burst: f64) -> Self {
        Self {
            rate,
            burst,
            state: Mutex::new((burst, Instant::now())),
        }
    }

    /// Take a token; returns how long the caller must wait before using it
    /// (zero when one was available).
    pub fn take(&self) -> Duration {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let refill = now.duration_since(st.1).as_secs_f64() * self.rate;
        st.0 = (st.0 + refill).min(self.burst);
        st.1 = now;
        st.0 -= 1.0;
        if st.0 >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-st.0 / self.rate)
        }
    }

    /// Take a token and sleep until it is due.
    pub fn acquire(&self) {
        let wait = self.take();
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// The one limiter every OBS network call passes through.
pub fn obs_acquire() {
    static BUCKET: std::sync::OnceLock<TokenBucket> = std::sync::OnceLock::new();
    BUCKET
        .get_or_init(|| {
            let rps =
                crate::env::env_positive_f64("AUTO_PACKAGE_OBS_RPS").unwrap_or(DEFAULT_OBS_RPS);
            TokenBucket::new(rps, OBS_BURST)
        })
        .acquire();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_free_then_calls_wait_for_the_rate() {
        let b = TokenBucket::new(5.0, 10.0);
        for _ in 0..10 {
            assert!(b.take().is_zero());
        }
        let w = b.take();
        assert!(
            w > Duration::from_millis(100) && w <= Duration::from_millis(200),
            "{w:?}"
        );
        // Waits queue up: the next caller waits longer than the previous one.
        assert!(b.take() > w);
    }

    #[test]
    fn the_package_limit_counts_distinct_names() {
        assert!(try_start_package("limit-a", 2));
        assert!(try_start_package("limit-b", 2));
        assert!(
            try_start_package("limit-a", 2),
            "a retry is not a new package"
        );
        assert!(!try_start_package("limit-c", 2));
        assert!(try_start_package("limit-c", 0), "0 is unlimited");
    }

    #[test]
    fn the_daily_cap_defaults_and_can_be_set() {
        assert_eq!(max_total_usd(), DEFAULT_MAX_TOTAL_USD);
        set_max_total_usd(7.5);
        assert_eq!(max_total_usd(), 7.5);
        set_max_total_usd(DEFAULT_MAX_TOTAL_USD);
    }
}
