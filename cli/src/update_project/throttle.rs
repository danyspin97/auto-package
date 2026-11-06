//! How far the preparation of a project run may get ahead of its builds.
//!
//! Branching a package queues a build of it in the branch project, so a
//! preparation that branched every found package at once would fill the
//! project's OBS queue long before most of them are built. The preparation
//! asks the [`Throttle`] for room before it branches each item: it gets it
//! while fewer than `2 x jobs` handed-over items have not ended their run,
//! counting the items of the unit it is preparing (a package and the
//! dependencies `--update-deps` adds for it). A unit is handed over whole,
//! so when nothing else is in flight its next item is always let through,
//! however big the unit is: otherwise it would wait for itself.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// How often a waiting preparation looks whether the run is stopping.
const STOP_POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    /// Items handed over whose run has not ended.
    held: usize,
    /// Items of the unit being prepared that were let through.
    unit: usize,
}

pub(super) struct Throttle {
    /// The most items in flight; `None` for no limit.
    cap: Option<usize>,
    state: Mutex<State>,
    cvar: Condvar,
    stopping: Box<dyn Fn() -> bool + Send + Sync>,
}

impl Throttle {
    /// Room for `2 x jobs` items; `jobs` 0 is no limit. `stopping` says
    /// whether the run is stopping, which releases a waiting preparation.
    pub(super) fn new(jobs: usize, stopping: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Throttle {
            cap: (jobs != 0).then(|| jobs.saturating_mul(2)),
            state: Mutex::new(State::default()),
            cvar: Condvar::new(),
            stopping: Box::new(stopping),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn room_in(&self, s: &State) -> bool {
        match self.cap {
            None => true,
            Some(cap) => s.held == 0 || s.held + s.unit < cap,
        }
    }

    /// Whether the next item of the unit would be let through now.
    #[cfg(test)]
    fn room(&self) -> bool {
        self.room_in(&self.lock())
    }

    /// Wait for room for the next item of the unit being prepared, and take
    /// it. `false` when the run is stopping: nothing is taken then.
    pub(super) fn admit(&self) -> bool {
        let mut s = self.lock();
        loop {
            if (self.stopping)() {
                return false;
            }
            if self.room_in(&s) {
                s.unit += 1;
                return true;
            }
            s = self
                .cvar
                .wait_timeout(s, STOP_POLL)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// An item let through by [`Throttle::admit`] is not handed over after
    /// all (it failed, or was skipped): its place goes back to the unit, and a
    /// waiting preparation may take it.
    pub(super) fn dropped(&self) {
        let mut s = self.lock();
        s.unit = s.unit.saturating_sub(1);
        drop(s);
        self.cvar.notify_all();
    }

    /// The unit being prepared is handed over as `jobs` jobs, each of which
    /// ends its run once: the next unit starts from nothing.
    pub(super) fn handed_over(&self, jobs: usize) {
        let mut s = self.lock();
        s.held += jobs;
        s.unit = 0;
    }

    /// The run of a handed-over job ended, however it ended.
    pub(super) fn run_ended(&self) {
        let mut s = self.lock();
        s.held = s.held.saturating_sub(1);
        drop(s);
        self.cvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn never() -> Throttle {
        Throttle::new(2, || false)
    }

    #[test]
    fn items_are_let_through_up_to_twice_the_jobs() {
        let t = never();
        for _ in 0..4 {
            assert!(t.admit());
            t.handed_over(1);
        }
        assert!(!t.room());
    }

    #[test]
    fn the_items_of_the_unit_being_prepared_count() {
        let t = never();
        assert!(t.admit());
        t.handed_over(1);
        for _ in 0..3 {
            assert!(t.admit());
        }
        assert!(!t.room());
    }

    #[test]
    fn a_waiting_preparation_goes_on_when_a_run_ends() {
        let t = Arc::new(never());
        for _ in 0..4 {
            assert!(t.admit());
        }
        t.handed_over(4);
        assert!(!t.room());
        let waiter = {
            let t = t.clone();
            std::thread::spawn(move || t.admit())
        };
        t.run_ended();
        assert!(waiter.join().unwrap());
        // The one that ended made room for one item, which is taken now.
        assert!(!t.room());
    }

    #[test]
    fn a_dropped_item_gives_its_place_back() {
        let t = never();
        assert!(t.admit());
        t.handed_over(1);
        for _ in 0..3 {
            assert!(t.admit());
        }
        assert!(!t.room());
        t.dropped();
        assert!(t.room());
        assert!(t.admit());
        assert!(!t.room());
    }

    #[test]
    fn a_dropped_item_releases_a_waiting_preparation() {
        let t = Arc::new(never());
        assert!(t.admit());
        t.handed_over(1);
        for _ in 0..3 {
            assert!(t.admit());
        }
        let waiter = {
            let t = t.clone();
            std::thread::spawn(move || t.admit())
        };
        t.dropped();
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn dropping_more_than_admitted_does_not_underflow() {
        let t = never();
        t.dropped();
        assert!(t.admit());
        t.dropped();
        t.dropped();
        assert!(t.room());
    }

    #[test]
    fn a_unit_bigger_than_the_limit_is_let_through_alone() {
        let t = never();
        for _ in 0..10 {
            assert!(t.admit());
        }
        t.handed_over(10);
        // Its items in flight hold back the next unit until they drop under
        // the limit.
        for _ in 0..6 {
            t.run_ended();
        }
        assert!(!t.room());
        t.run_ended();
        assert!(t.admit());
        // The next unit's second item waits while anything else is in
        // flight, and grows past the limit once nothing is.
        assert!(!t.room());
        for _ in 0..3 {
            t.run_ended();
        }
        for _ in 0..10 {
            assert!(t.room());
            assert!(t.admit());
        }
    }

    #[test]
    fn no_jobs_limit_never_waits() {
        let t = Throttle::new(0, || false);
        for _ in 0..1000 {
            assert!(t.admit());
        }
        t.handed_over(1000);
        assert!(t.room());
        assert!(t.admit());
    }

    #[test]
    fn stopping_releases_a_waiting_preparation() {
        let stop = Arc::new(AtomicBool::new(false));
        let t = {
            let stop = stop.clone();
            Arc::new(Throttle::new(1, move || stop.load(Ordering::SeqCst)))
        };
        assert!(t.admit());
        assert!(t.admit());
        t.handed_over(2);
        let waiter = {
            let t = t.clone();
            std::thread::spawn(move || t.admit())
        };
        stop.store(true, Ordering::SeqCst);
        // Released without any run ending, and given no room.
        assert!(!waiter.join().unwrap());
        assert!(!t.room());
    }
}
