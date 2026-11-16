//! Disk safety for downloads and extraction.
//!
//! Work that writes a lot (an archive download, an unpack of up to a few GiB)
//! announces how much it expects to write by taking a [`Lease`] from the shared
//! [`Reserve`]. A lease is refused when the free space of the target filesystem,
//! minus what leases already held may still write, minus a safety margin, would
//! go below zero. The error is typed ([`NotEnoughSpace`]) so callers can tell a
//! full disk from any other failure.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Default free space always left alone.
pub const DEFAULT_MARGIN: u64 = 1 << 30;
/// Default number of extractions running at once.
pub const DEFAULT_EXTRACTIONS: usize = 2;

/// The safety margin in bytes: `AUTO_PACKAGE_DISK_MARGIN_GB` (a positive number,
/// fractions allowed) or [`DEFAULT_MARGIN`].
pub fn margin() -> u64 {
    ap_core::env::env_positive_f64("AUTO_PACKAGE_DISK_MARGIN_GB")
        .map_or(DEFAULT_MARGIN, |gb| (gb * (1u64 << 30) as f64) as u64)
}

/// There is not enough free space for the work asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotEnoughSpace {
    /// What was about to be written, for the message.
    pub what: String,
    /// Bytes the work expects to write.
    pub needed: u64,
    /// Free bytes on the target filesystem.
    pub free: u64,
    /// Bytes other in-flight work may still write.
    pub reserved: u64,
    /// The safety margin.
    pub margin: u64,
}

impl std::fmt::Display for NotEnoughSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mib = |b: u64| b >> 20;
        write!(
            f,
            "not enough free space for {}: needs {} MiB, {} MiB free, {} MiB reserved by \
             work in flight, {} MiB kept as a safety margin",
            self.what,
            mib(self.needed),
            mib(self.free),
            mib(self.reserved),
            mib(self.margin)
        )
    }
}

impl std::error::Error for NotEnoughSpace {}

/// Whether `needed` bytes fit: `free - reserved - margin - needed >= 0`.
/// `free` of `None` (the filesystem cannot say) always fits.
pub fn check(free: Option<u64>, reserved: u64, margin: u64, needed: u64) -> Result<(), (u64, u64)> {
    let Some(free) = free else {
        return Ok(());
    };
    let spoken_for = reserved.saturating_add(margin).saturating_add(needed);
    if free >= spoken_for {
        Ok(())
    } else {
        Err((free, reserved))
    }
}

/// The bytes in-flight work may still write, shared by every thread.
#[derive(Debug, Default)]
pub struct Reserve {
    held: Mutex<u64>,
}

impl Reserve {
    /// Reserve `needed` bytes for `what` on the filesystem holding `dir`,
    /// reading its free space with `free`. The check and the reservation are
    /// one step, so two callers cannot both pass on the same free bytes.
    pub fn try_lease(
        self: &Arc<Self>,
        what: &str,
        dir: &Path,
        needed: u64,
        margin: u64,
        free: impl FnOnce(&Path) -> Option<u64>,
    ) -> Result<Lease, NotEnoughSpace> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        check(free(dir), *held, margin, needed).map_err(|(free, reserved)| NotEnoughSpace {
            what: what.to_string(),
            needed,
            free,
            reserved,
            margin,
        })?;
        *held = held.saturating_add(needed);
        Ok(Lease {
            reserve: Arc::clone(self),
            bytes: needed,
        })
    }

    /// Bytes currently reserved.
    pub fn held(&self) -> u64 {
        *self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Bytes reserved against the free space until dropped.
#[derive(Debug)]
pub struct Lease {
    reserve: Arc<Reserve>,
    bytes: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut held = self.reserve.held.lock().unwrap_or_else(|e| e.into_inner());
        *held = held.saturating_sub(self.bytes);
    }
}

/// The reserve shared by the whole process.
pub fn global() -> &'static Arc<Reserve> {
    static RESERVE: OnceLock<Arc<Reserve>> = OnceLock::new();
    RESERVE.get_or_init(Default::default)
}

/// Reserve `needed` bytes on the filesystem holding `dir` from the global
/// reserve, using the real free space and the configured margin.
pub fn lease(what: &str, dir: &Path, needed: u64) -> Result<Lease, NotEnoughSpace> {
    global().try_lease(what, dir, needed, margin(), ap_core::fsx::free_bytes)
}

/// A counting semaphore.
#[derive(Debug)]
pub struct Semaphore {
    permits: Mutex<usize>,
    freed: Condvar,
}

impl Semaphore {
    pub fn new(permits: usize) -> Self {
        Semaphore {
            permits: Mutex::new(permits.max(1)),
            freed: Condvar::new(),
        }
    }

    /// Wait for a permit; it is given back when the guard drops.
    pub fn acquire(&self) -> Permit<'_> {
        let mut n = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        while *n == 0 {
            n = self.freed.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n -= 1;
        Permit(self)
    }
}

#[derive(Debug)]
pub struct Permit<'a>(&'a Semaphore);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.permits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.freed.notify_one();
    }
}

/// Wait for one of the process's extraction slots:
/// `AUTO_PACKAGE_EXTRACTIONS` at once (default [`DEFAULT_EXTRACTIONS`]).
pub fn extraction_slot() -> Permit<'static> {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| {
        let n = ap_core::env::env_positive_f64("AUTO_PACKAGE_EXTRACTIONS")
            .map_or(DEFAULT_EXTRACTIONS, |n| n as usize);
        Semaphore::new(n)
    })
    .acquire()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn check_subtracts_reserve_margin_and_need() {
        assert!(check(Some(100), 30, 20, 50).is_ok());
        assert_eq!(check(Some(100), 30, 20, 51), Err((100, 30)));
        assert!(check(None, u64::MAX, u64::MAX, u64::MAX).is_ok());
        assert!(check(Some(0), 0, 0, 0).is_ok());
        // No overflow however large the figures.
        assert!(check(Some(10), u64::MAX, u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn leases_count_against_each_other_and_are_released_on_drop() {
        let reserve = Arc::new(Reserve::default());
        let dir = Path::new("/x");
        let free = |_: &Path| Some(1000 * MIB);
        let a = reserve
            .try_lease("a", dir, 400 * MIB, 100 * MIB, free)
            .unwrap();
        let b = reserve
            .try_lease("b", dir, 400 * MIB, 100 * MIB, free)
            .unwrap();
        assert_eq!(reserve.held(), 800 * MIB);
        // 1000 - 800 - 100 leaves 100: 101 does not fit, 100 does.
        let err = reserve
            .try_lease("c", dir, 101 * MIB, 100 * MIB, free)
            .unwrap_err();
        assert_eq!(
            (err.free, err.reserved, err.needed, err.margin),
            (1000 * MIB, 800 * MIB, 101 * MIB, 100 * MIB)
        );
        assert!(err.to_string().contains("not enough free space for c"));
        drop(a);
        assert_eq!(reserve.held(), 400 * MIB);
        let c = reserve
            .try_lease("c", dir, 101 * MIB, 100 * MIB, free)
            .unwrap();
        drop((b, c));
        assert_eq!(reserve.held(), 0);
    }

    #[test]
    fn a_refused_lease_reserves_nothing() {
        let reserve = Arc::new(Reserve::default());
        assert!(
            reserve
                .try_lease("x", Path::new("/"), 10, 0, |_| Some(5))
                .is_err()
        );
        assert_eq!(reserve.held(), 0);
    }

    #[test]
    fn the_free_space_is_read_for_the_given_directory() {
        let reserve = Arc::new(Reserve::default());
        let seen = Mutex::new(None);
        let _ = reserve.try_lease("x", Path::new("/some/dir"), 1, 0, |d| {
            *seen.lock().unwrap() = Some(d.to_path_buf());
            Some(10)
        });
        assert_eq!(seen.into_inner().unwrap().unwrap(), Path::new("/some/dir"));
    }

    #[test]
    fn the_semaphore_never_lets_more_than_its_permits_run() {
        let sem = Arc::new(Semaphore::new(2));
        let running = Arc::new(Mutex::new((0usize, 0usize)));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (sem, running) = (sem.clone(), running.clone());
                std::thread::spawn(move || {
                    let _p = sem.acquire();
                    {
                        let mut r = running.lock().unwrap();
                        r.0 += 1;
                        r.1 = r.1.max(r.0);
                    }
                    std::thread::yield_now();
                    running.lock().unwrap().0 -= 1;
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let (now, peak) = *running.lock().unwrap();
        assert_eq!(now, 0);
        assert!((1..=2).contains(&peak));
    }

    #[test]
    fn margin_defaults_to_a_gibibyte() {
        if std::env::var_os("AUTO_PACKAGE_DISK_MARGIN_GB").is_none() {
            assert_eq!(margin(), DEFAULT_MARGIN);
        }
    }
}
