//! How many forge calls a run makes (plan 3.3). Only uncached calls count;
//! the cap is smaller without a token, and a rate-limit answer stops the rest
//! of the run's calls.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Calls allowed without a token (GitHub gives 60 an hour per address).
pub const UNAUTHENTICATED: usize = 40;
/// Calls allowed with a token or `gh` login.
pub const AUTHENTICATED: usize = 1000;

const SPENT: &str = "the GitHub API budget of this run is spent; log in with `gh auth login` \
or set GITHUB_TOKEN";
const LIMITED: &str = "the forge's rate limit was hit earlier in this run";

/// A count of calls against a cap, and a stop after a rate-limit answer.
pub struct Budget {
    calls: AtomicUsize,
    limited: AtomicBool,
}

impl Budget {
    pub const fn new() -> Budget {
        Budget {
            calls: AtomicUsize::new(0),
            limited: AtomicBool::new(false),
        }
    }

    /// Count one call against `cap`; an error once the cap is spent or a rate
    /// limit was seen.
    pub fn spend_within(&self, cap: usize) -> Result<(), String> {
        if self.limited.load(Ordering::Relaxed) {
            return Err(LIMITED.to_string());
        }
        if self.calls.fetch_add(1, Ordering::Relaxed) >= cap {
            return Err(SPENT.to_string());
        }
        Ok(())
    }

    pub fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.limited.store(false, Ordering::Relaxed);
    }

    /// Stop the rest of the run's calls when `error` is a rate-limit answer.
    pub fn note_error(&self, error: &str) {
        let lower = error.to_ascii_lowercase();
        // The texts the forge client gives: its refusal (`rate-limited`, the
        // unauthenticated quota), a 429 (a rate limit by itself), or a 403 that
        // says `rate limit`.
        let refused = error.contains("rate-limited") || error.contains("req/hour");
        let limited =
            error.contains("429") || (error.contains("403") && lower.contains("rate limit"));
        if refused || limited {
            self.limited.store(true, Ordering::Relaxed);
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

static RUN: Budget = Budget::new();

/// Whether a token or a `gh` login is known: the larger cap applies.
pub fn authenticated() -> bool {
    let token = ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|t| !t.is_empty()));
    token || gh_logged_in()
}

/// Whether `gh` is installed and logged in, asked once per process.
fn gh_logged_in() -> bool {
    static LOGGED_IN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOGGED_IN.get_or_init(|| {
        std::process::Command::new("gh")
            .args(["auth", "status"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Count one call of the run, against the cap its authentication allows.
pub fn spend() -> Result<(), String> {
    let cap = if authenticated() {
        AUTHENTICATED
    } else {
        UNAUTHENTICATED
    };
    RUN.spend_within(cap)
}

/// Starts a run's budget afresh: one invocation of `update` or `lint` is one
/// run, however many projects or packages it covers.
pub fn reset() {
    RUN.reset();
}

/// Tell the run's budget about a failed call. The stop is sticky for the whole
/// process (not per host): one rate limit ends the run's later calls, which is
/// the conservative choice for a run that asks a single forge.
pub fn note_error(error: &str) {
    RUN.note_error(error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_429_on_any_route_stops_the_later_calls() {
        let b = Budget::new();
        b.note_error("https://example.org/x.patch: http status: 429");
        assert!(b.spend_within(10).is_err());
        let b = Budget::new();
        b.note_error("https://example.org/x.patch: http status: 403");
        assert!(b.spend_within(10).is_ok());
    }

    #[test]
    fn the_call_after_the_cap_is_refused() {
        let b = Budget::new();
        for _ in 0..UNAUTHENTICATED {
            assert!(b.spend_within(UNAUTHENTICATED).is_ok());
        }
        assert!(b.spend_within(UNAUTHENTICATED).is_err());
    }

    #[test]
    fn the_forge_client_refusal_texts_stop_the_later_calls() {
        for text in [
            "https://api.github.com/x: http status: 403 (GitHub unauthenticated REST is 60 req/hour per IP; install the `gh` CLI)",
            "https://gitlab.com/x: rate-limited (set GITLAB_TOKEN for authenticated access)",
        ] {
            let b = Budget::new();
            b.note_error(text);
            assert!(b.spend_within(10).is_err(), "{text}");
        }
        let b = Budget::new();
        b.note_error("https://api.github.com/x: http status: 404");
        assert!(b.spend_within(10).is_ok());
    }

    #[test]
    fn a_rate_limit_answer_stops_the_later_calls() {
        let b = Budget::new();
        b.note_error("404 not found");
        assert!(b.spend_within(10).is_ok());
        b.note_error("403 forbidden: API rate limit exceeded");
        assert!(b.spend_within(10).is_err());
    }
}
