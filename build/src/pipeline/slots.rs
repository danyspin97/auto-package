//! Agent slots and rate limits: the agent job slots, the rate limit pause, and the
//! in-flight guards that keep a package from running twice.

use super::*;

/// How many times one turn is re-issued when the API answers 429. Each retry
/// first waits out the process-wide pause (see `AgentSlot::note_rate_limited`),
/// so the retries of parallel packages do not all hit the quota at once. Past
/// this the failure goes to the round loop, which retries the package next round.
pub(super) const MAX_IMMEDIATE_RATE_LIMIT_RETRIES: u32 = 5;

/// Agent turns running at once, process-wide. Unlimited by default so packages
/// get built as fast as the API allows; set `AUTO_PACKAGE_AGENT_JOBS=<n>` to cap
/// it if the model quota (429s) is the bottleneck. An invalid value means the
/// default cap, not unlimited.
static AGENT_ACTIVE: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// The quota is one pool shared by every package, so a 429 from any turn pauses
/// all of them: no turn starts before `until`. Each consecutive 429 lengthens
/// the pause, and a turn that gets through resets it.
struct RateLimit {
    until: Option<std::time::Instant>,
    streak: u32,
}

static AGENT_RATE_LIMIT: std::sync::Mutex<RateLimit> = std::sync::Mutex::new(RateLimit {
    until: None,
    streak: 0,
});

/// The pause after the `streak`-th consecutive 429: 2s, doubling, capped at 60s.
pub(super) fn rate_limit_pause(streak: u32) -> std::time::Duration {
    let secs = 2u64 << streak.saturating_sub(1).min(5);
    std::time::Duration::from_secs(secs.min(60))
}

/// Cap used when `AUTO_PACKAGE_AGENT_JOBS` is set to something unusable.
pub(super) const DEFAULT_AGENT_JOBS: usize = 8;

/// The cap `AUTO_PACKAGE_AGENT_JOBS` asks for and whether the value was usable.
/// An unusable value gives the default cap, never "unlimited".
pub(super) fn parse_agent_jobs(raw: Option<&str>) -> (Option<usize>, bool) {
    match raw.map(str::trim) {
        None | Some("") => (None, true),
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => (Some(n), true),
            _ => (Some(DEFAULT_AGENT_JOBS), false),
        },
    }
}

pub(super) struct AgentSlot;

impl AgentSlot {
    /// `None` = no cap (only when the variable is unset or empty).
    pub(super) fn limit() -> Option<usize> {
        let raw = std::env::var_os("AUTO_PACKAGE_AGENT_JOBS");
        let (cap, valid) = parse_agent_jobs(raw.as_deref().and_then(|v| v.to_str()));
        if !valid {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "warning: AUTO_PACKAGE_AGENT_JOBS is not a positive number; \
                     using the default cap of {DEFAULT_AGENT_JOBS}"
                );
            });
        }
        cap
    }

    /// Record a 429: no turn starts again before the pause for it has passed.
    pub(super) fn note_rate_limited() {
        let mut rl = AGENT_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
        rl.streak = rl.streak.saturating_add(1);
        let until = std::time::Instant::now() + rate_limit_pause(rl.streak);
        rl.until = Some(rl.until.map_or(until, |u| u.max(until)));
    }

    /// A turn got through, so the next 429 starts the pause from the beginning.
    pub(super) fn note_turn_ok() {
        AGENT_RATE_LIMIT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streak = 0;
    }

    /// Sleep until any shared pause has passed. Checked again after each sleep,
    /// since another package's 429 may have extended it in the meantime.
    fn wait_out_rate_limit() {
        loop {
            let until = AGENT_RATE_LIMIT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .until;
            let Some(until) = until else {
                return;
            };
            let now = std::time::Instant::now();
            if now >= until || crate::proc::shutting_down() {
                return;
            }
            std::thread::sleep((until - now).min(std::time::Duration::from_secs(1)));
        }
    }

    pub(super) fn acquire() -> Self {
        Self::wait_out_rate_limit();
        let (lock, cvar) = &AGENT_ACTIVE;
        let max = Self::limit();
        let mut active = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(max) = max {
            while *active >= max {
                active = cvar.wait(active).unwrap_or_else(|e| e.into_inner());
            }
        }
        *active += 1;
        AgentSlot
    }
}

impl Drop for AgentSlot {
    fn drop(&mut self) {
        let (lock, cvar) = &AGENT_ACTIVE;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        cvar.notify_one();
    }
}

/// Join a resolve-phase worker. A panic is an error, not an empty answer.
pub(super) fn join_lookup<T>(
    h: std::thread::ScopedJoinHandle<'_, T>,
    what: &str,
) -> Result<T, String> {
    h.join()
        .map_err(|_| format!("the {what} lookup thread panicked"))
}

// ── main pipeline ─────────────────────────────────────────────────────────────

/// RPM names currently inside `run_package` anywhere in this process.
///
/// Several root builds can share one process (the TUI lets the user add more
/// at runtime), and two of them can discover the same dependency. Both would
/// then drive the same checkout, state dir and OBS package at once. Holding
/// this guard serialises them: the second waits, then its own `is_built`
/// check turns a success into `AlreadyBuilt` instead of a duplicate build.
static IN_FLIGHT: (
    std::sync::Mutex<std::collections::BTreeSet<String>>,
    std::sync::Condvar,
) = (
    std::sync::Mutex::new(std::collections::BTreeSet::new()),
    std::sync::Condvar::new(),
);

pub(super) struct InFlightGuard(String);

impl InFlightGuard {
    pub(super) fn acquire(rpm_name: &str) -> Self {
        let (lock, cvar) = &IN_FLIGHT;
        let mut busy = lock.lock().unwrap_or_else(|e| e.into_inner());
        while busy.contains(rpm_name) {
            busy = cvar.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        busy.insert(rpm_name.to_string());
        Self(rpm_name.to_string())
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &IN_FLIGHT;
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
        cvar.notify_all();
    }
}

/// A build can run for a long time, so a second process waits this long for
/// the first to finish before giving up.
pub(super) const BUILD_LOCK_WAIT: Duration = Duration::from_secs(30 * 60);

pub(super) fn lock_error_text(e: &ap_core::proc_lock::LockError) -> String {
    match e {
        ap_core::proc_lock::LockError::Held(Some(pid)) => format!("held by pid {pid}"),
        ap_core::proc_lock::LockError::Held(None) => "held by another process".to_string(),
        ap_core::proc_lock::LockError::Io(e) => e.to_string(),
    }
}
