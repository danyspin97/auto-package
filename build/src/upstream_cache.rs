//! The newest upstream version of a package, kept on disk for a while.
//!
//! An `update` sweep asks PyPI or a git forge for every package of a project.
//! Doing that again a few minutes later, for the real run after a `--dry-run`
//! or after a failure, gains nothing and spends the forge's rate limit. A
//! version asked for within [`TTL`] is read back from one file in the work
//! directory instead. Only answers are kept, never errors.
//!
//! New answers are kept in memory and written in batches: every
//! [`SAVE_EVERY`] new entries and by [`flush`], which the sweep calls when it
//! ends and the process calls on its way out. A write reads what is on disk
//! first and merges it (under a file lock), so two processes sweeping at once
//! keep each other's entries.
//!
//! Without [`set_dir`] nothing is read or written, so library callers opt in
//! explicitly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a version stays valid. A release published inside the window is
/// found by the next run after it, or at once with `--refresh`.
pub const TTL: Duration = Duration::from_secs(60 * 60);

/// Bumped whenever the lookups change what they answer for the same key (a
/// directory listing read another way, a tag filtered differently), so the
/// answers an older lookup stored are asked again instead of served.
pub const LOGIC_VERSION: u32 = 2;

/// New entries kept in memory before the file is written.
pub const SAVE_EVERY: usize = 50;

const FILE: &str = "upstream-latest.json";
const LOCK_FILE: &str = "upstream-latest.lock";
/// How long a save waits for another process's save to finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    version: String,
    /// Seconds since the epoch.
    at: u64,
    /// The [`LOGIC_VERSION`] that computed it; entries written before it was
    /// kept read back as 0.
    #[serde(default)]
    logic: u32,
}

impl Entry {
    fn fresh(&self, now: u64) -> bool {
        self.logic == LOGIC_VERSION && now.saturating_sub(self.at) < TTL.as_secs()
    }
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static REFRESH: AtomicBool = AtomicBool::new(false);
static HITS: AtomicUsize = AtomicUsize::new(0);
static FETCHED: AtomicUsize = AtomicUsize::new(0);
static STORE: Mutex<Option<Store>> = Mutex::new(None);

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

fn load(dir: &Path) -> HashMap<String, Entry> {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The entries in memory, and which of them this process has added since the
/// last save.
struct Store {
    dir: PathBuf,
    entries: HashMap<String, Entry>,
    unsaved: HashSet<String>,
}

impl Store {
    fn open(dir: &Path) -> Store {
        Store {
            dir: dir.to_path_buf(),
            entries: load(dir),
            unsaved: HashSet::new(),
        }
    }

    /// The version kept for `key` when it is still fresh at `now` (seconds
    /// since the epoch).
    fn fresh(&self, key: &str, now: u64) -> Option<String> {
        self.entries
            .get(key)
            .filter(|e| e.fresh(now))
            .map(|e| e.version.clone())
    }

    /// Keep an answer; `true` when enough are waiting to be written.
    fn put(&mut self, key: &str, version: &str, now: u64) -> bool {
        self.entries.insert(
            key.to_string(),
            Entry {
                version: version.to_string(),
                at: now,
                logic: LOGIC_VERSION,
            },
        );
        self.unsaved.insert(key.to_string());
        self.unsaved.len() >= SAVE_EVERY
    }

    /// Write what this process added, merged into what is on disk now, under a
    /// file lock so another process's save is not overwritten. Entries past
    /// their time are dropped, so the file does not grow with every package
    /// ever checked. A cache that cannot be written is only slower, never
    /// wrong: the entries stay unsaved for the next try.
    fn save(&mut self, now: u64) {
        if self.unsaved.is_empty() {
            return;
        }
        let Ok(_lock) = ap_core::proc_lock::lock_with_wait(&self.dir.join(LOCK_FILE), LOCK_WAIT)
        else {
            return;
        };
        let mut merged = load(&self.dir);
        for key in &self.unsaved {
            let Some(mine) = self.entries.get(key) else {
                continue;
            };
            if merged.get(key).is_none_or(|theirs| theirs.at <= mine.at) {
                merged.insert(key.clone(), mine.clone());
            }
        }
        merged.retain(|_, e| e.fresh(now));
        let Ok(text) = serde_json::to_string(&merged) else {
            return;
        };
        if ap_core::fsx::write_atomic(&self.dir.join(FILE), text).is_ok() {
            // What other processes saved is now known here too.
            self.entries = merged;
            self.unsaved.clear();
        }
    }
}

/// The version `fetch` finds for `key` (`pypi:<name>` or `repo:<url>`), or the
/// one kept from the last [`TTL`].
pub fn lookup(key: &str, fetch: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    let Some(dir) = DIR.get() else {
        return fetch();
    };
    let t = now();
    if !REFRESH.load(Ordering::Relaxed) {
        let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
        let store = guard.get_or_insert_with(|| Store::open(dir));
        if let Some(version) = store.fresh(key, t) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(version);
        }
    }
    let version = fetch()?;
    FETCHED.fetch_add(1, Ordering::Relaxed);
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(|| Store::open(dir));
    if store.put(key, &version, t) {
        store.save(t);
    }
    Ok(version)
}

/// Write the answers not yet on disk. Called when a sweep ends and when the
/// process exits; harmless when there is nothing to write.
pub fn flush() {
    if let Some(store) = STORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        store.save(now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path) -> HashMap<String, Entry> {
        load(dir)
    }

    #[test]
    fn a_stored_entry_is_fresh_only_within_the_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path());
        store.put("pypi:a", "1.0", 10_000);
        let ttl = TTL.as_secs();
        assert_eq!(store.fresh("pypi:a", 10_000).as_deref(), Some("1.0"));
        assert_eq!(
            store.fresh("pypi:a", 10_000 + ttl - 1).as_deref(),
            Some("1.0")
        );
        assert_eq!(store.fresh("pypi:a", 10_000 + ttl), None);
        assert_eq!(store.fresh("pypi:other", 10_000), None);
        // A clock that went backwards does not make it stale.
        assert_eq!(store.fresh("pypi:a", 5_000).as_deref(), Some("1.0"));
    }

    #[test]
    fn an_answer_of_an_older_lookup_logic_is_not_fresh() {
        let current = Entry {
            version: "1".into(),
            at: 100,
            logic: LOGIC_VERSION,
        };
        assert!(current.fresh(101));
        let old = Entry {
            logic: LOGIC_VERSION - 1,
            ..current.clone()
        };
        assert!(!old.fresh(101));
        // A file written before the field existed has no `logic` at all.
        let legacy: Entry = serde_json::from_str(r#"{"version":"1","at":100}"#).unwrap();
        assert!(!legacy.fresh(101));
    }

    #[test]
    fn a_saved_file_is_read_back_by_the_next_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path());
        assert!(store.entries.is_empty());
        store.put("pypi:requests", "2.32.0", 5);
        store.save(5);
        let next = Store::open(dir.path());
        assert_eq!(next.fresh("pypi:requests", 6).as_deref(), Some("2.32.0"));
        assert!(load(&dir.path().join("missing")).is_empty());
        // A file that is not JSON is an empty cache, not a failure.
        std::fs::write(dir.path().join(FILE), "{broken").unwrap();
        assert!(Store::open(dir.path()).entries.is_empty());
    }

    #[test]
    fn nothing_is_written_until_enough_answers_wait_or_a_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path());
        for i in 0..SAVE_EVERY - 1 {
            assert!(!store.put(&format!("k{i}"), "1", 100), "{i}");
        }
        assert!(!dir.path().join(FILE).exists());
        assert!(store.put("last", "1", 100));
        store.save(100);
        assert_eq!(read(dir.path()).len(), SAVE_EVERY);
        assert!(store.unsaved.is_empty());
        // Nothing new: nothing written (the file stays as it is).
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        store.save(100);
        assert!(!dir.path().join(FILE).exists());
        // A flush writes a single waiting answer.
        store.put("one", "2", 100);
        store.save(100);
        assert!(read(dir.path()).contains_key("one"));
    }

    #[test]
    fn two_processes_do_not_lose_each_others_entries() {
        let dir = tempfile::tempdir().unwrap();
        // Both open the (empty) cache, then each learns something.
        let mut a = Store::open(dir.path());
        let mut b = Store::open(dir.path());
        a.put("pypi:a", "1", 100);
        b.put("pypi:b", "2", 100);
        a.save(100);
        b.save(100);
        let on_disk = read(dir.path());
        assert_eq!(on_disk["pypi:a"].version, "1");
        assert_eq!(on_disk["pypi:b"].version, "2");
        // The saver also learned what the other had written.
        assert_eq!(b.fresh("pypi:a", 101).as_deref(), Some("1"));
    }

    #[test]
    fn the_newer_answer_wins_a_merge_and_stale_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Store::open(dir.path());
        let mut b = Store::open(dir.path());
        a.put("k", "new", 200);
        a.put("old", "x", 1);
        let almost = 200 + TTL.as_secs() - 1;
        a.save(almost);
        // `b` has an older answer for `k`: the one on disk stays.
        b.put("k", "older", 150);
        b.save(almost);
        let on_disk = read(dir.path());
        assert_eq!(on_disk["k"].version, "new");
        // An entry past its TTL is not written.
        assert!(!on_disk.contains_key("old"));
        let later = 200 + TTL.as_secs();
        let mut c = Store::open(dir.path());
        c.put("fresh", "1", later);
        c.save(later);
        assert!(!read(dir.path()).contains_key("k"));
    }

    #[test]
    fn a_save_that_cannot_write_keeps_the_entries_for_later() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("gone"));
        store.put("k", "1", 100);
        store.save(100);
        assert!(store.unsaved.contains("k"));
        assert_eq!(store.fresh("k", 101).as_deref(), Some("1"));
    }
}
