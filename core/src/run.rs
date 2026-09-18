use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
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
}

impl RunDir {
    /// Allocate a new, empty run directory.
    pub fn allocate(work_dir: &Path) -> io::Result<Self> {
        let run_id = new_run_id();
        let path = work_dir.join("runs").join(&run_id.0);
        fs::create_dir_all(&path)?;
        Ok(Self { path, run_id })
    }

    pub fn jsonl(&self) -> PathBuf {
        self.path.join("run.jsonl")
    }

    pub fn summary(&self) -> PathBuf {
        self.path.join("run.json")
    }

    pub fn pkg(&self, rpm_name: &str) -> PkgRunDir {
        PkgRunDir {
            path: self.path.join(sanitize_name(rpm_name)),
            counter: AtomicU32::new(0),
        }
    }
}

/// Per-package directory within a run: `<run-dir>/<rpm-name>/`.
pub struct PkgRunDir {
    pub path: PathBuf,
    counter: AtomicU32,
}

impl PkgRunDir {
    /// Ensure the package directory exists.
    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.path)
    }

    /// Allocate the next numbered step directory: `<pkg-dir>/<NN>-<label>/`.
    pub fn step(&self, label: &str) -> io::Result<StepDir> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let dir_name = format!("{:02}-{}", n + 1, sanitize_name(label));
        let path = self.path.join(dir_name);
        fs::create_dir_all(&path)?;
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
