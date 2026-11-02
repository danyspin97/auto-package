//! The bounded wait for the other-architecture builds to settle.

use std::time::Duration;

/// Poll until no result is pending any more, or `bound` has passed. Between
/// two polls it pauses for `delay(unchanged)`, the count of polls that saw the
/// same results in a row, never past the time left. The results of the last
/// poll come back either way: a result still pending then is "still building".
///
/// `poll` reads the results, `pause` sleeps, and `elapsed` says how long the
/// wait has run, so the clock and the sleeps are the caller's (and a test's).
pub fn wait_settled<K: Clone + PartialEq>(
    bound: Duration,
    mut poll: impl FnMut() -> Result<Vec<(K, crate::osc::BuildStatus)>, String>,
    mut pause: impl FnMut(Duration),
    mut elapsed: impl FnMut() -> Duration,
    mut delay: impl FnMut(u32) -> Duration,
) -> Result<Vec<(K, crate::osc::BuildStatus)>, String> {
    let mut unchanged: u32 = 0;
    let mut last: Option<Vec<(K, crate::osc::BuildStatus)>> = None;
    loop {
        let now = poll()?;
        let pending = now.iter().any(|(_, s)| !s.is_terminal());
        let left = bound.saturating_sub(elapsed());
        if !pending || left.is_zero() || crate::proc::shutting_down() {
            return Ok(now);
        }
        unchanged = if last.as_ref() == Some(&now) {
            unchanged.saturating_add(1)
        } else {
            0
        };
        pause(delay(unchanged).min(left));
        last = Some(now);
    }
}
