//! The newest upstream version of a package, kept on disk for a while.
//!
//! An `update` sweep asks PyPI or a git forge for every package of a project.
//! Doing that again a few minutes later, for the real run after a `--dry-run`
//! or after a failure, gains nothing and spends the forge's rate limit. A
//! version asked for within [`TTL`] is read back from one file in the work
//! directory instead. Only answers are kept, never errors.
//!
//! Without [`set_dir`] nothing is read or written, so library callers opt in
//! explicitly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a version stays valid. A release published inside the window is
/// found by the next run after it, or at once with `--refresh`.
pub const TTL: Duration = Duration::from_secs(60 * 60);

const FILE: &str = "upstream-latest.json";

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    version: String,
    /// Seconds since the epoch.
    at: u64,
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static REFRESH: AtomicBool = AtomicBool::new(false);
static HITS: AtomicUsize = AtomicUsize::new(0);
static FETCHED: AtomicUsize = AtomicUsize::new(0);
static ENTRIES: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

/// Keep the cache file in `dir`.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// Ask upstream for every version, ignoring what is kept (the answers are
/// still stored).
pub fn set_refresh(refresh: bool) {
    REFRESH.store(refresh, Ordering::Relaxed);
}

/// How many versions came from the cache and how many were asked upstream, so
/// far in this process.
pub fn stats() -> (usize, usize) {
    (
        HITS.load(Ordering::Relaxed),
        FETCHED.load(Ordering::Relaxed),
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn load(dir: &std::path::Path) -> HashMap<String, Entry> {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The version `fetch` finds for `key` (`pypi:<name>` or `repo:<url>`), or the
/// one kept from the last [`TTL`].
pub fn lookup(key: &str, fetch: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    let Some(dir) = DIR.get() else {
        return fetch();
    };
    let t = now();
    if !REFRESH.load(Ordering::Relaxed) {
        let mut guard = ENTRIES.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.get_or_insert_with(|| load(dir));
        if let Some(e) = entries.get(key)
            && t.saturating_sub(e.at) < TTL.as_secs()
        {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(e.version.clone());
        }
    }
    let version = fetch()?;
    FETCHED.fetch_add(1, Ordering::Relaxed);
    let mut guard = ENTRIES.lock().unwrap_or_else(|e| e.into_inner());
    let entries = guard.get_or_insert_with(|| load(dir));
    entries.insert(
        key.to_string(),
        Entry {
            version: version.clone(),
            at: t,
        },
    );
    // Entries past their time are dropped as the file is rewritten, so it does
    // not grow with every package ever checked.
    entries.retain(|_, e| t.saturating_sub(e.at) < TTL.as_secs());
    // A cache that cannot be written is only slower, never wrong.
    if let Ok(text) = serde_json::to_string(&*entries) {
        let _ = ap_core::fsx::write_atomic(&dir.join(FILE), text);
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_entry_is_fresh_only_within_the_ttl() {
        let t = 10_000;
        let fresh = |at: u64| t - at < TTL.as_secs();
        assert!(fresh(t - 10));
        assert!(!fresh(t - TTL.as_secs()));
    }

    #[test]
    fn the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = HashMap::new();
        m.insert(
            "pypi:requests".to_string(),
            Entry {
                version: "2.32.0".into(),
                at: 5,
            },
        );
        std::fs::write(dir.path().join(FILE), serde_json::to_string(&m).unwrap()).unwrap();
        let back = load(dir.path());
        assert_eq!(back["pypi:requests"].version, "2.32.0");
        assert!(load(&dir.path().join("missing")).is_empty());
    }
}
