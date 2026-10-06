use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{RunId, StepId};

/// Sanitize a name for use as a directory component.
/// Keeps alphanumeric, `-`, `_`, `.`; replaces everything else with `_`.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Generate a run ID: `YYYYMMDD-HHMMSS-<6hexchars>`.
pub fn new_run_id() -> RunId {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    let sub = now.subsec_nanos() & 0x00FF_FFFF;
    RunId(format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}-{sub:06x}"))
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970u64;
    loop {
        let ydays = if is_leap(y) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        y += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// The immutable directory for one run: `<work-dir>/runs/<run-id>/`.
pub struct RunDir {
    pub path: PathBuf,
    pub run_id: RunId,
    /// Shared step counters, keyed by rpm name. The outer build loop calls
    /// `pkg(rpm_name)` once per retry — if each call allocated a fresh
    /// counter, two attempts of the same package would both allocate
    /// `01-build/`, `02-fix/`, etc., and the second attempt's log would
    /// overwrite the first. Sharing an `Arc<AtomicU32>` per rpm name
    /// across all `PkgRunDir`s for that name means retries get the next
    /// number (`03-build/`, …) and no attempt's artifacts can be lost.
    pkg_counters: Mutex<HashMap<String, Arc<AtomicU32>>>,
}

impl RunDir {
    /// Allocate a new, empty run directory.
    pub fn allocate(work_dir: &Path) -> io::Result<Self> {
        let run_id = new_run_id();
        let path = work_dir.join("runs").join(&run_id.0);
        fs::create_dir_all(&path)?;
        Ok(Self {
            path,
            run_id,
            pkg_counters: Mutex::new(HashMap::new()),
        })
    }

    pub fn jsonl(&self) -> PathBuf {
        self.path.join("run.jsonl")
    }

    pub fn summary(&self) -> PathBuf {
        self.path.join("run.json")
    }

    pub fn pkg(&self, rpm_name: &str) -> PkgRunDir {
        let counter = self
            .pkg_counters
            .lock()
            .unwrap()
            .entry(rpm_name.to_string())
            .or_insert_with(|| Arc::new(AtomicU32::new(0)))
            .clone();
        PkgRunDir {
            path: self.path.join(sanitize_name(rpm_name)),
            counter,
        }
    }
}

/// Per-package directory within a run: `<run-dir>/<rpm-name>/`.
pub struct PkgRunDir {
    pub path: PathBuf,
    /// Shared with every other `PkgRunDir` for the same rpm name in the
    /// same `RunDir` — see `RunDir::pkg_counters`'s doc comment for why
    /// shared across retries matters.
    counter: Arc<AtomicU32>,
}

impl PkgRunDir {
    /// Ensure the package directory exists.
    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.path)
    }

    /// Allocate the next numbered step directory: `<pkg-dir>/<NN>-<label>/`.
    ///
    /// Uses `create_dir` (not `create_dir_all`) so a step dir that already
    /// exists is a hard failure instead of silently reused — reusing a
    /// step dir lets a later attempt's `build.log` overwrite an earlier
    /// one's, which the earlier review probe reproduced. Combined with
    /// `RunDir::pkg_counters` sharing the counter across retries, two
    /// attempts of the same package get distinct numbered dirs
    /// (`01-build/`, `02-build/`) rather than colliding.
    pub fn step(&self, label: &str) -> io::Result<StepDir> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let dir_name = format!("{:02}-{}", n + 1, sanitize_name(label));
        let path = self.path.join(dir_name);
        match fs::create_dir(&path) {
            Ok(()) => {}
            // The parent directory may not exist yet (callers often skip
            // calling `ensure()`); let that case through so it isn't a
            // regression. Any other error — including AlreadyExists —
            // is still fatal.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&path)?;
            }
            Err(e) => return Err(e),
        }
        Ok(StepDir { path, id: StepId(n) })
    }
}

/// One numbered step directory within a package run dir.
pub struct StepDir {
    pub path: PathBuf,
    pub id: StepId,
}

impl StepDir {
    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Earlier review probe: two attempts of the same package allocated
    /// the same `01-build/build.log` because each `RunDir::pkg()` call
    /// handed back a fresh counter starting at zero. Retries must get
    /// distinct numbered step dirs so the first attempt's artifacts
    /// survive long enough to be inspected.
    #[test]
    fn step_counters_are_shared_across_pkg_calls_for_the_same_rpm_name() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        // First attempt
        let p1 = run.pkg("python-foo");
        p1.ensure().unwrap();
        let s1 = p1.step("build").unwrap();
        assert!(s1.path.ends_with("01-build"));

        // Retry: a new PkgRunDir handle for the same package. The counter
        // is shared, so the next step is 02, not 01 — the first attempt's
        // 01-build/ directory is preserved.
        let p2 = run.pkg("python-foo");
        let s2 = p2.step("build").unwrap();
        assert!(s2.path.ends_with("02-build"), "got {:?}", s2.path);
        assert_ne!(s1.path, s2.path, "retry must not reuse the first attempt's dir");
    }

    /// Two different packages must have independent counters — a build of
    /// `python-foo` must not consume a step number that then shifts
    /// `python-bar`'s indices.
    #[test]
    fn step_counters_are_per_rpm_name() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        let foo = run.pkg("python-foo");
        foo.ensure().unwrap();
        let _ = foo.step("build").unwrap();
        let _ = foo.step("fix").unwrap();

        let bar = run.pkg("python-bar");
        bar.ensure().unwrap();
        let s = bar.step("build").unwrap();
        assert!(s.path.ends_with("01-build"), "got {:?}", s.path);
    }

    /// `step()` must refuse to hand back a directory that already exists
    /// — the pre-fix `create_dir_all` quietly reused it and the next
    /// write to `build.log` overwrote whatever was there.
    #[test]
    fn step_refuses_to_reuse_an_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        let p1 = run.pkg("python-foo");
        p1.ensure().unwrap();
        let s = p1.step("build").unwrap();
        std::fs::write(s.file("build.log"), b"first attempt").unwrap();

        // Simulate a code path that somehow allocated the same step name
        // again (counter corruption, external interference). The second
        // attempt must fail hard instead of overwriting the first.
        //
        // In practice the shared counter prevents this at the allocation
        // layer; the create_dir fallthrough is the belt-and-braces check.
        p1.counter.store(0, Ordering::SeqCst);
        let err = p1
            .step("build")
            .err()
            .expect("re-allocating 01-build must fail");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
}
