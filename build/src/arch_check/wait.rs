//! The bounded wait for the other-architecture builds to settle, and the one
//! deadline the checks of a run share.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Poll until no result is pending any more, or `left` says no time is left.
/// Between two polls it pauses for `delay(unchanged)`, the count of polls that
/// saw the same results in a row, never past the time left. `left` is asked
/// after each poll, so a deadline that moves is followed. The results of the
/// last poll come back either way: a result still pending then is "still
/// building".
///
/// `poll` reads the results, `pause` sleeps, and `delay` says how long to pause,
/// so the clock and the sleeps are the caller's (and a test's).
pub fn wait_settled<K: Clone + PartialEq>(
    mut left: impl FnMut() -> Duration,
    mut poll: impl FnMut() -> Result<Vec<(K, crate::osc::BuildStatus)>, String>,
    mut pause: impl FnMut(Duration),
    mut delay: impl FnMut(u32) -> Duration,
) -> Result<Vec<(K, crate::osc::BuildStatus)>, String> {
    let mut unchanged: u32 = 0;
    let mut last: Option<Vec<(K, crate::osc::BuildStatus)>> = None;
    loop {
        let now = poll()?;
        let pending = now.iter().any(|(_, s)| !s.is_terminal());
        let left = left();
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

/// The deadline of the other-architecture waits of a run. It starts when a
/// build of the run ends and lasts `bound` from there, so the checks of all
/// the packages wait for OBS together: N packages do not wait N times `bound`.
/// The last build to end sets it.
#[derive(Debug)]
pub struct ArchDeadline {
    bound: Duration,
    until: Mutex<Option<Instant>>,
}

impl ArchDeadline {
    pub const fn new(bound: Duration) -> Self {
        ArchDeadline {
            bound,
            until: Mutex::new(None),
        }
    }

    /// A build of the run ended at `at`: the deadline is at least `bound` after it.
    pub fn build_ended(&self, at: Instant) {
        let mut until = self.until.lock().unwrap_or_else(|p| p.into_inner());
        *until = Some(deadline_after(at, self.bound, *until));
    }

    /// A deadline that starts at `at`, `bound` after it: for a wait that is not
    /// tied to a build of the run, such as a submit batch with none of its own.
    pub fn starting_at(bound: Duration, at: Instant) -> Self {
        let deadline = Self::new(bound);
        deadline.build_ended(at);
        deadline
    }

    /// Whether the deadline has started: a build of the run has ended, or it
    /// was made by [`ArchDeadline::starting_at`].
    pub fn started(&self) -> bool {
        self.until
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// The time the waits may still go on at `now`.
    pub fn left(&self, now: Instant) -> Duration {
        let until = *self.until.lock().unwrap_or_else(|p| p.into_inner());
        time_left(until, self.bound, now)
    }
}

/// The deadline a submit's other-architecture wait ends by: the run's once a
/// build of the run has ended, else `own`, the batch's own deadline (see
/// [`ArchDeadline::starting_at`]), so the waits of one batch end together.
pub fn submit_deadline<'a>(run: &'a ArchDeadline, own: &'a ArchDeadline) -> &'a ArchDeadline {
    if run.started() { run } else { own }
}

/// The deadline after a build ended at `ended`, given the one already set: the
/// later of the two, so a build that ends earlier never shortens the wait.
pub fn deadline_after(ended: Instant, bound: Duration, current: Option<Instant>) -> Instant {
    let own = ended + bound;
    current.map_or(own, |c| c.max(own))
}

/// The time left before `until` at `now`. With no deadline yet, the whole
/// `bound`: a check that runs before any build ended waits as before.
pub fn time_left(until: Option<Instant>, bound: Duration, now: Instant) -> Duration {
    until.map_or(bound, |u| u.saturating_duration_since(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUND: Duration = Duration::from_secs(1800);

    #[test]
    fn the_deadline_is_the_bound_after_the_build_that_ends_last() {
        let t0 = Instant::now();
        let d = ArchDeadline::new(BOUND);
        assert_eq!(d.left(t0), BOUND, "no build ended yet: the whole bound");
        d.build_ended(t0);
        assert_eq!(d.left(t0), BOUND);
        assert_eq!(d.left(t0 + BOUND / 2), BOUND / 2);
        assert_eq!(d.left(t0 + BOUND), Duration::ZERO);
        assert_eq!(d.left(t0 + BOUND * 2), Duration::ZERO);
    }

    #[test]
    fn a_later_build_moves_the_deadline_and_an_earlier_one_does_not() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(600);
        let d = ArchDeadline::new(BOUND);
        d.build_ended(t1);
        d.build_ended(t0);
        assert_eq!(d.left(t1), BOUND, "the earlier end does not shorten it");
        let t2 = t0 + Duration::from_secs(900);
        d.build_ended(t2);
        assert_eq!(d.left(t1), t2.duration_since(t1) + BOUND);
    }

    #[test]
    fn a_submit_waits_for_the_run_once_a_build_ended_and_for_its_own_before() {
        let t0 = Instant::now();
        let run = ArchDeadline::new(BOUND);
        let own = ArchDeadline::starting_at(BOUND, t0);
        assert!(!run.started());
        assert!(std::ptr::eq(submit_deadline(&run, &own), &own));
        let t1 = t0 + Duration::from_secs(600);
        run.build_ended(t1);
        assert!(std::ptr::eq(submit_deadline(&run, &own), &run));
        assert_eq!(submit_deadline(&run, &own).left(t1), BOUND);
    }

    #[test]
    fn the_packages_of_a_batch_without_a_build_share_one_bound() {
        let t0 = Instant::now();
        let own = ArchDeadline::starting_at(BOUND, t0);
        assert!(own.started());
        // The first package waits for the whole bound; a later one, after
        // twenty minutes, gets what is left of that same bound, not a new one.
        assert_eq!(own.left(t0), BOUND);
        assert_eq!(own.left(t0 + Duration::from_secs(1200)), BOUND / 3);
        assert_eq!(own.left(t0 + BOUND), Duration::ZERO);
    }

    #[test]
    fn the_deadline_arithmetic_is_pure() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(100);
        assert_eq!(deadline_after(t0, BOUND, None), t0 + BOUND);
        assert_eq!(
            deadline_after(t0, Duration::from_secs(10), Some(later)),
            later,
            "an earlier end does not shorten the deadline"
        );
        assert_eq!(
            deadline_after(later, BOUND, Some(t0)),
            later + BOUND,
            "a later end wins over an earlier deadline"
        );
        assert_eq!(time_left(None, BOUND, t0), BOUND);
        assert_eq!(time_left(Some(t0), BOUND, later), Duration::ZERO);
        assert_eq!(time_left(Some(later), BOUND, t0), Duration::from_secs(100));
    }
}
