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

use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// How long the answer stays fresh, in seconds.
    #[serde(default = "default_ttl_secs")]
    ttl: u64,
}

fn default_ttl_secs() -> u64 {
    TTL.as_secs()
}

impl Entry {
    fn fresh(&self, now: u64) -> bool {
        self.logic == LOGIC_VERSION && now.saturating_sub(self.at) < self.ttl
    }
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static REFRESH: AtomicBool = AtomicBool::new(false);
static HITS: AtomicUsize = AtomicUsize::new(0);
static FETCHED: AtomicUsize = AtomicUsize::new(0);
static STORE: Mutex<Option<Store>> = Mutex::new(None);
/// The failures of this run by key. Kept only in memory: a transient error may
/// be gone by the next run, so it is not written to the file.
static FAILED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

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

/// The entries of the file. A file that is not a JSON object is an empty cache;
/// an entry that does not read is dropped alone, so the others keep their time.
fn load(dir: &Path) -> HashMap<String, Entry> {
    let Some(text) = std::fs::read_to_string(dir.join(FILE)).ok() else {
        return HashMap::new();
    };
    let Ok(raw) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&text) else {
        return HashMap::new();
    };
    raw.into_iter()
        .filter_map(|(key, value)| Some((key, serde_json::from_value(value).ok()?)))
        .collect()
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

    /// Keep an answer for [`TTL`]; `true` when enough are waiting to be written.
    #[cfg(test)]
    fn put(&mut self, key: &str, version: &str, now: u64) -> bool {
        self.put_for(key, version, now, TTL)
    }

    /// Keep an answer for `ttl`; `true` when enough are waiting to be written.
    fn put_for(&mut self, key: &str, version: &str, now: u64, ttl: Duration) -> bool {
        self.entries.insert(
            key.to_string(),
            Entry {
                version: version.to_string(),
                at: now,
                logic: LOGIC_VERSION,
                ttl: ttl.as_secs(),
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
    let refresh = REFRESH.load(Ordering::Relaxed);
    lookup_in(&STORE, dir, key, TTL, now(), refresh, fetch)
}

/// [`lookup`] for an answer kept for `ttl`. `refresh` asks `fetch` anyway; the
/// global `--refresh` does not apply, the caller decides. The answer is kept
/// either way, and a failure never is.
pub fn lookup_with_ttl(
    key: &str,
    ttl: Duration,
    refresh: bool,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let Some(dir) = DIR.get() else {
        return fetch();
    };
    lookup_in(&STORE, dir, key, ttl, now(), refresh, fetch)
}

/// The lookup behind [`lookup_with_ttl`], with the cache and the clock given:
/// `now` is seconds since the epoch.
fn lookup_in(
    cache: &Mutex<Option<Store>>,
    dir: &Path,
    key: &str,
    ttl: Duration,
    now: u64,
    refresh: bool,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if !refresh {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        let store = guard.get_or_insert_with(|| Store::open(dir));
        if let Some(version) = store.fresh(key, now) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(version);
        }
    }
    let version = fetch()?;
    FETCHED.fetch_add(1, Ordering::Relaxed);
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(|| Store::open(dir));
    if store.put_for(key, &version, now, ttl) {
        store.save(now);
    }
    Ok(version)
}

/// [`lookup`] for a source that may be asked for many packages of one run
/// (Repology). A failure is kept for the rest of the process, so the same key
/// is not asked again; it is not kept across runs.
pub fn once_per_run(
    key: &str,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    ask_once(&FAILED, key, fetch)
}

/// Ask `fetch` for `key` unless `failed` already holds a failure for it. A new
/// failure is recorded there; an answer is not (the cache file keeps those).
pub(crate) fn ask_once(
    failed: &Mutex<BTreeMap<String, String>>,
    key: &str,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let known = failed
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned();
    if let Some(error) = known {
        return Err(error);
    }
    let answer = fetch();
    if let Err(error) = &answer {
        failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), error.clone());
    }
    answer
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
            ttl: TTL.as_secs(),
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
    fn an_answer_is_fresh_for_the_ttl_it_was_kept_with() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path());
        store.put_for("short", "1", 100, Duration::from_secs(60));
        store.put_for("long", "2", 100, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(store.fresh("short", 159).as_deref(), Some("1"));
        assert_eq!(store.fresh("short", 160), None);
        assert_eq!(store.fresh("long", 160).as_deref(), Some("2"));
        store.save(160);
        // Saving keeps the long answer and drops the short one.
        let on_disk = read(dir.path());
        assert!(!on_disk.contains_key("short"));
        assert!(on_disk.contains_key("long"));
    }

    #[test]
    fn an_entry_without_a_ttl_keeps_the_default_ttl() {
        // Written before the field existed: fresh for the default TTL.
        let legacy: Entry = serde_json::from_str(r#"{"version":"1","at":100,"logic":2}"#).unwrap();
        assert!(legacy.fresh(100 + TTL.as_secs() - 1));
        assert!(!legacy.fresh(100 + TTL.as_secs()));
    }

    #[test]
    fn one_bad_entry_does_not_discard_the_others() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            r#"{"good":{"version":"1","at":100,"logic":2,"ttl":3600},"bad":{"version":5}}"#,
        )
        .unwrap();
        let entries = load(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries["good"].version, "1");
    }

    #[test]
    fn a_lookup_asks_again_only_after_its_ttl_and_never_keeps_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Mutex::new(None);
        let ttl = Duration::from_secs(3600);
        let asked = std::cell::Cell::new(0);
        let fetch = |v: &'static str| {
            asked.set(asked.get() + 1);
            Ok(v.to_string())
        };
        let got = lookup_in(&cache, dir.path(), "k", ttl, 1000, false, || fetch("a"));
        assert_eq!(got.as_deref(), Ok("a"));
        // Within the TTL: kept, no fetch.
        let got = lookup_in(&cache, dir.path(), "k", ttl, 1000 + 3599, false, || {
            fetch("b")
        });
        assert_eq!(got.as_deref(), Ok("a"));
        assert_eq!(asked.get(), 1);
        // Past the TTL: fetched again.
        let got = lookup_in(&cache, dir.path(), "k", ttl, 1000 + 3600, false, || {
            fetch("c")
        });
        assert_eq!(got.as_deref(), Ok("c"));
        // `refresh` fetches although the answer is fresh.
        let got = lookup_in(&cache, dir.path(), "k", ttl, 4000, true, || fetch("d"));
        assert_eq!(got.as_deref(), Ok("d"));
        assert_eq!(asked.get(), 3);
        // A failed fetch is not kept: the next call asks again.
        let failing = || Err("503".to_string());
        assert_eq!(
            lookup_in(&cache, dir.path(), "f", ttl, 5000, false, failing),
            Err("503".into())
        );
        let got = lookup_in(&cache, dir.path(), "f", ttl, 5001, false, || fetch("e"));
        assert_eq!(got.as_deref(), Ok("e"));
        assert_eq!(asked.get(), 4);
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

    #[test]
    fn a_failure_is_asked_once_per_run() {
        let failed = Mutex::new(BTreeMap::new());
        let asked = std::cell::Cell::new(0);
        let fetch = || {
            asked.set(asked.get() + 1);
            Err("unknown project".to_string())
        };
        assert_eq!(
            ask_once(&failed, "repology:x", fetch),
            Err("unknown project".into())
        );
        let again = || -> Result<String, String> { unreachable!("asked again") };
        assert_eq!(
            ask_once(&failed, "repology:x", again),
            Err("unknown project".into())
        );
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_failure_of_one_key_does_not_hold_another() {
        let failed = Mutex::new(BTreeMap::new());
        let _ = ask_once(&failed, "repology:x", || Err("no".to_string()));
        assert_eq!(
            ask_once(&failed, "repology:y", || Ok("1.0".to_string())),
            Ok("1.0".into())
        );
    }

    #[test]
    fn an_answer_is_not_kept_here() {
        let failed = Mutex::new(BTreeMap::new());
        let asked = std::cell::Cell::new(0);
        for _ in 0..2 {
            let fetch = || {
                asked.set(asked.get() + 1);
                Ok("2.0".to_string())
            };
            assert_eq!(ask_once(&failed, "repology:x", fetch), Ok("2.0".into()));
        }
        assert_eq!(asked.get(), 2);
    }
}
