//! The persistent spend ledger: `<work-dir>/ledger.json`, USD per UTC day.
//!
//! The cap is per day across runs and processes, so every read-modify-write
//! happens under an exclusive `flock` on `ledger.json.lock` (the lock helper
//! rewrites its file, so it cannot be the ledger itself). Everything fails
//! closed: a ledger that cannot be read, parsed or locked refuses the turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::proc_lock::{LockError, lock_with_wait};

const LOCK_WAIT: Duration = Duration::from_secs(10);

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    /// `YYYY-MM-DD` (UTC) to USD spent that day.
    days: BTreeMap<String, f64>,
}

/// The ledger file in a work directory.
pub fn path(work_dir: &Path) -> PathBuf {
    work_dir.join("ledger.json")
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn utc_day_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_day(secs)
}

/// The UTC date of a unix timestamp.
pub fn utc_day(secs: u64) -> String {
    // Civil-from-days (Howard Hinnant).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn lock(work_dir: &Path) -> Result<std::fs::File, String> {
    let lock_path = work_dir.join("ledger.json.lock");
    lock_with_wait(&lock_path, LOCK_WAIT).map_err(|e| match e {
        LockError::Held(pid) => format!(
            "the spend ledger {} is locked{}",
            lock_path.display(),
            pid.map(|p| format!(" by pid {p}")).unwrap_or_default()
        ),
        LockError::Io(e) => format!("cannot lock the spend ledger {}: {e}", lock_path.display()),
    })
}

fn read(work_dir: &Path) -> Result<Ledger, String> {
    let p = path(work_dir);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse the spend ledger {}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(e) => Err(format!("cannot read the spend ledger {}: {e}", p.display())),
    }
}

/// Add `usd` to today's total.
pub fn record(work_dir: &Path, usd: f64) -> Result<(), String> {
    record_on(work_dir, &utc_day_now(), usd)
}

fn record_on(work_dir: &Path, day: &str, usd: f64) -> Result<(), String> {
    if !(usd.is_finite() && usd > 0.0) {
        return Ok(());
    }
    let _guard = lock(work_dir)?;
    let mut ledger = read(work_dir)?;
    *ledger.days.entry(day.to_string()).or_insert(0.0) += usd;
    let json = serde_json::to_string_pretty(&ledger).map_err(|e| e.to_string())?;
    crate::fsx::write_atomic(&path(work_dir), json)
        .map_err(|e| format!("cannot write the spend ledger: {e}"))
}

/// Today's total.
pub fn today_total(work_dir: &Path) -> Result<f64, String> {
    total_on(work_dir, &utc_day_now())
}

fn total_on(work_dir: &Path, day: &str) -> Result<f64, String> {
    let _guard = lock(work_dir)?;
    Ok(read(work_dir)?.days.get(day).copied().unwrap_or(0.0))
}

/// Refuse (with a message naming the daily cap) when today's total plus
/// `estimate_usd` would exceed `cap_usd`, or when the ledger cannot be used.
pub fn check(work_dir: &Path, estimate_usd: f64, cap_usd: f64) -> Result<(), String> {
    check_on(work_dir, &utc_day_now(), estimate_usd, cap_usd)
}

fn check_on(work_dir: &Path, day: &str, estimate_usd: f64, cap_usd: f64) -> Result<(), String> {
    let total =
        total_on(work_dir, day).map_err(|e| format!("{e}; refusing to spend (fail closed)"))?;
    if total + estimate_usd > cap_usd {
        return Err(format!(
            "the daily spend cap of ${cap_usd:.2} (--max-total-usd) is reached: ${total:.2} spent \
             on {day} (UTC) plus about ${estimate_usd:.2} for this turn"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_782_518_400), "2026-06-27");
        assert_eq!(utc_day(951_782_400 + 86_399), "2000-02-29");
    }

    #[test]
    fn spend_accumulates_per_day_and_the_cap_applies() {
        let dir = tempfile::tempdir().unwrap();
        record_on(dir.path(), "2026-01-01", 30.0).unwrap();
        record_on(dir.path(), "2026-01-01", 19.0).unwrap();
        record_on(dir.path(), "2026-01-02", 1.0).unwrap();
        assert_eq!(total_on(dir.path(), "2026-01-01").unwrap(), 49.0);
        assert!(check_on(dir.path(), "2026-01-01", 0.5, 50.0).is_ok());
        let err = check_on(dir.path(), "2026-01-01", 1.5, 50.0).unwrap_err();
        assert!(err.contains("daily spend cap of $50.00"), "{err}");
        // A new day starts at zero.
        assert!(check_on(dir.path(), "2026-01-03", 1.5, 50.0).is_ok());
    }

    #[test]
    fn an_unreadable_ledger_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "not json").unwrap();
        let err = check_on(dir.path(), "2026-01-01", 0.0, 50.0).unwrap_err();
        assert!(err.contains("fail closed"), "{err}");
        assert!(record_on(dir.path(), "2026-01-01", 1.0).is_err());
        // The corrupt file is left alone for the owner to look at.
        assert_eq!(
            std::fs::read_to_string(path(dir.path())).unwrap(),
            "not json"
        );
    }

    #[test]
    fn a_missing_work_directory_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope");
        assert!(check_on(&gone, "2026-01-01", 0.0, 50.0).is_err());
    }

    #[test]
    fn a_held_lock_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let _held = crate::proc_lock::try_lock(&dir.path().join("ledger.json.lock")).unwrap();
        // lock_with_wait waits 10s; that is the documented bound.
        assert!(total_on(dir.path(), "2026-01-01").is_err());
    }
}
