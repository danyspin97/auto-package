//! Worker threads of a build: the concurrency limit, the panic guard and joins.

use crate::*;

/// A counting semaphore that limits how many package builds run concurrently.
///
/// `None` means unlimited. One instance is created per Build/Update command
/// and shared by every `cmd_build` it spawns (including TUI-added roots), so
/// `--jobs` is a global cap. Threads block in `acquire` until a slot is free.
pub(crate) struct Limiter {
    limit: usize,
    active: std::sync::Mutex<usize>,
    cvar: std::sync::Condvar,
}

pub(crate) type Semaphore = Arc<Limiter>;

pub(crate) fn make_semaphore(jobs: usize) -> Option<Semaphore> {
    (jobs != 0).then(|| {
        Arc::new(Limiter {
            limit: jobs,
            active: std::sync::Mutex::new(0),
            cvar: std::sync::Condvar::new(),
        })
    })
}

/// A held semaphore slot, released on drop so a panicking build cannot leak it.
pub(crate) struct Permit(Option<Semaphore>);

impl Permit {
    pub(crate) fn acquire(sem: &Option<Semaphore>) -> Permit {
        if let Some(s) = sem {
            // A poisoned lock only means another holder panicked; the count
            // itself is still consistent because release happens in `drop`.
            let mut active = s.active.lock().unwrap_or_else(|e| e.into_inner());
            while *active >= s.limit {
                active = s.cvar.wait(active).unwrap_or_else(|e| e.into_inner());
            }
            *active += 1;
        }
        Permit(sem.clone())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            *s.active.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
            s.cvar.notify_one();
        }
    }
}

/// Best-effort message from a `catch_unwind` / `JoinHandle::join` panic payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Run `f`, converting a panic into an `Err` carrying the panic message.
pub(crate) fn catch_panic<T>(f: impl FnOnce() -> color_eyre::Result<T>) -> color_eyre::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        Err(color_eyre::eyre::eyre!(
            "build panicked: {}",
            panic_message(p.as_ref())
        ))
    })
}

/// Join a package thread, turning a panic into a per-package failure so one
/// crashed build does not take the whole run down.
pub(crate) fn join_package(
    handle: std::thread::JoinHandle<PipelineResult>,
    rpm_name: &str,
) -> PipelineResult {
    handle.join().unwrap_or_else(|p| PipelineResult::Failed {
        rpm_name: rpm_name.to_string(),
        reason: format!("build thread panicked: {}", panic_message(p.as_ref())),
        kind: FailureKind::Other,
    })
}
