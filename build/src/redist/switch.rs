use super::*;

pub(super) static ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    /// Per-thread value for tests, which run in parallel in one process.
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Turn the mode on or off. Set once at startup from `--nvidia-redist`.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    #[cfg(test)]
    if let Some(v) = TEST_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    ENABLED.load(Ordering::Relaxed)
}

/// Holds the mode at a value on the current thread until dropped.
#[cfg(test)]
pub struct TestMode(Option<bool>);

#[cfg(test)]
impl TestMode {
    pub fn set(on: bool) -> Self {
        TestMode(TEST_OVERRIDE.with(|c| c.replace(Some(on))))
    }
}

#[cfg(test)]
impl Drop for TestMode {
    fn drop(&mut self) {
        TEST_OVERRIDE.with(|c| c.set(self.0));
    }
}
