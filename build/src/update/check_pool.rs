//! Checks the packages of a project on a bounded pool of threads, and hands
//! the answers on in package order.
//!
//! Each check is mostly waiting: an `osc` call per package and a few upstream
//! requests. A pool runs several of them at once. The answers still come out
//! in the order of `names`, so the plan, its output and the candidates handed
//! to the caller are the same whatever the pool size; only the time differs.

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, Once, mpsc};

use super::Candidate;

/// Packages checked at once when nothing says otherwise.
pub const DEFAULT_JOBS: usize = 8;

/// The environment variable that sets the pool size (`--check-jobs` wins).
pub const JOBS_ENV: &str = "AUTO_PACKAGE_CHECK_JOBS";

/// Set by `--check-jobs`; 0 when not given.
static JOBS: AtomicUsize = AtomicUsize::new(0);

/// Use `n` packages at a time for the check (1 is the sequential check); 0
/// leaves the choice to [`JOBS_ENV`] and [`DEFAULT_JOBS`].
pub fn set_check_jobs(n: usize) {
    JOBS.store(n, Ordering::Relaxed);
}

/// The pool size of this run: `--check-jobs`, else [`JOBS_ENV`], else
/// [`DEFAULT_JOBS`]. Never 0.
pub fn check_jobs() -> usize {
    match JOBS.load(Ordering::Relaxed) {
        0 => jobs_from_env(std::env::var(JOBS_ENV).ok().as_deref()),
        n => n,
    }
}

/// The pool size an environment value asks for. Unset gives the default; a
/// value that is not a number of packages, 1 or more, gives the default too,
/// and is reported once.
pub(super) fn jobs_from_env(raw: Option<&str>) -> usize {
    static WARNED: Once = Once::new();
    let Some(text) = raw else {
        return DEFAULT_JOBS;
    };
    parse_jobs(text).unwrap_or_else(|| {
        WARNED.call_once(|| {
            ap_core::diag!(
                "{JOBS_ENV}={text:?} is not a number of packages, 1 or more: \
                 checking {DEFAULT_JOBS} at a time"
            );
        });
        DEFAULT_JOBS
    })
}

/// A pool size written as a decimal number of packages, 1 or more.
fn parse_jobs(text: &str) -> Option<usize> {
    text.trim().parse::<usize>().ok().filter(|n| *n > 0)
}

/// Run `check` on each of `names`, `jobs` at a time, calling `progress(i)`
/// and then `on(i, candidate)` for each name in order. `on` returns whether
/// to go on; `false` stops the check, and the names after the one it refused
/// are not handed on (checks already in flight finish, and are dropped).
///
/// A check that panics does not stop the others: `panicked(name, message)`
/// gives that name's answer instead.
///
/// With `jobs` of 1 this is the plain loop: `progress` runs before `check`.
/// With more, `progress` runs as the answer is handed on, so it counts the
/// packages done in order. A worker starts a name only when it is less than
/// `jobs` names past the answer being waited for, so at most `jobs` answers
/// are held here at once, and a slow name holds back the ones after it.
pub(super) fn check_in_order(
    names: &[String],
    jobs: usize,
    check: &(dyn Fn(&str) -> Candidate + Sync),
    panicked: &(dyn Fn(&str, &str) -> Candidate + Sync),
    progress: &dyn Fn(usize),
    on: &mut dyn FnMut(usize, Candidate) -> bool,
) {
    if jobs <= 1 {
        for (i, n) in names.iter().enumerate() {
            progress(i);
            if !on(i, guarded(n, check, panicked)) {
                break;
            }
        }
        return;
    }
    let state = Mutex::new(Window {
        next: 0,
        want: 0,
        stop: false,
    });
    let moved = Condvar::new();
    let (tx, rx) = mpsc::channel::<(usize, Candidate)>();
    std::thread::scope(|scope| {
        // Whatever leaves this closure (a panic in `on`, `progress` or a spawn),
        // the workers waiting in `claim` are let go, or the scope never ends.
        let _release = Release {
            state: &state,
            moved: &moved,
        };
        for _ in 0..jobs.min(names.len()) {
            let tx = tx.clone();
            let (state, moved) = (&state, &moved);
            scope.spawn(move || {
                while let Some(i) = claim(state, moved, names.len(), jobs) {
                    if tx.send((i, guarded(&names[i], check, panicked))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut waiting = BTreeMap::new();
        let mut want = 0;
        let mut going = true;
        for (i, candidate) in rx {
            if !going {
                continue;
            }
            waiting.insert(i, candidate);
            while let Some(c) = waiting.remove(&want) {
                progress(want);
                let keep = on(want, c);
                want += 1;
                let mut w = lock(&state);
                w.want = want;
                w.stop |= !keep;
                moved.notify_all();
                drop(w);
                if !keep {
                    going = false;
                    break;
                }
            }
        }
    });
}

/// Where the pool is: the next name to start, the next answer to hand on, and
/// whether the check was stopped.
struct Window {
    next: usize,
    want: usize,
    stop: bool,
}

/// The next name a worker may start, once it is less than `window` names past
/// the answer being waited for. `None` when the names run out or the check
/// was stopped. The answer being waited for is always startable, so the wait
/// cannot stall.
fn claim(state: &Mutex<Window>, moved: &Condvar, total: usize, window: usize) -> Option<usize> {
    let mut w = lock(state);
    loop {
        if w.stop || w.next >= total {
            return None;
        }
        if w.next < w.want + window {
            w.next += 1;
            return Some(w.next - 1);
        }
        w = moved.wait(w).unwrap_or_else(|e| e.into_inner());
    }
}

/// One name's answer. A check that panics gives `panicked` the name and the
/// panic's message, so the other names are still checked and handed on.
fn guarded(
    name: &str,
    check: &(dyn Fn(&str) -> Candidate + Sync),
    panicked: &(dyn Fn(&str, &str) -> Candidate + Sync),
) -> Candidate {
    catch_unwind(AssertUnwindSafe(|| check(name)))
        .unwrap_or_else(|payload| panicked(name, &panic_text(payload.as_ref())))
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_string())
}

/// Stops the check and wakes the workers when the main side leaves the scope.
struct Release<'a> {
    state: &'a Mutex<Window>,
    moved: &'a Condvar,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        lock(self.state).stop = true;
        self.moved.notify_all();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[path = "check_pool_tests.rs"]
mod tests;
