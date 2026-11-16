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

use crate::upstream_flights::{InFlight, ask_once};

/// How long a version stays valid. A release published inside the window is
/// found by the next run after it, or at once with `--refresh`.
pub const TTL: Duration = Duration::from_secs(60 * 60);

/// Bumped whenever the lookups change what they answer for the same key (a
/// directory listing read another way, a tag filtered differently), so the
/// answers an older lookup stored are asked again instead of served.
pub const LOGIC_VERSION: u32 = 3;

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
/// The requests in flight for the lookups and for the once-per-run asks.
static LOOKUP_FLIGHTS: InFlight = InFlight::new();
static ONCE_FLIGHTS: InFlight = InFlight::new();

/// Keep the cache file in `dir`.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// The work directory the cache was given, when one was set.
pub fn work_dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
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
pub fn lookup(
    key: &str,
    mut fetch: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    let Some(dir) = DIR.get() else {
        return fetch();
    };
    let refresh = REFRESH.load(Ordering::Relaxed);
    lookup_in(
        &STORE,
        dir,
        &LOOKUP_FLIGHTS,
        key,
        TTL,
        now(),
        refresh,
        fetch,
    )
}

/// Whether this run asked to refresh its answers (`--refresh`).
pub fn refreshing() -> bool {
    REFRESH.load(Ordering::Relaxed)
}

/// The answer kept for `key` while it is fresh, read without a fetch: `None`
/// when there is none, or no cache directory is set. An answer keeps the TTL
/// it was stored with.
pub fn fresh(key: &str) -> Option<String> {
    let dir = DIR.get()?;
    fresh_in(&STORE, dir, key, now())
}

fn fresh_in(cache: &Mutex<Option<Store>>, dir: &Path, key: &str, now: u64) -> Option<String> {
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(|| Store::open(dir));
    store.fresh(key, now)
}

/// [`lookup`] for an answer kept for `ttl`. `refresh` asks `fetch` anyway; the
/// global `--refresh` does not apply, the caller decides. The answer is kept
/// either way, and a failure never is.
pub fn lookup_with_ttl(
    key: &str,
    ttl: Duration,
    refresh: bool,
    mut fetch: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    let Some(dir) = DIR.get() else {
        return fetch();
    };
    lookup_in(
        &STORE,
        dir,
        &LOOKUP_FLIGHTS,
        key,
        ttl,
        now(),
        refresh,
        fetch,
    )
}

/// The lookup behind [`lookup_with_ttl`], with the cache, the requests in
/// flight and the clock given: `now` is seconds since the epoch. Callers that
/// ask for one key at once share one request; a shared request that failed is
/// asked again by each caller that waited for it, as a sequential caller would
/// have asked after a failure.
fn lookup_in(
    cache: &Mutex<Option<Store>>,
    dir: &Path,
    flights: &InFlight,
    key: &str,
    ttl: Duration,
    now: u64,
    refresh: bool,
    mut fetch: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    loop {
        if !refresh {
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            let store = guard.get_or_insert_with(|| Store::open(dir));
            if let Some(version) = store.fresh(key, now) {
                HITS.fetch_add(1, Ordering::Relaxed);
                return Ok(version);
            }
        }
        let (answer, led) = flights.run(key, || {
            let version = fetch()?;
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            let store = guard.get_or_insert_with(|| Store::open(dir));
            if store.put_for(key, &version, now, ttl) {
                store.save(now);
            }
            Ok(version)
        });
        match (&answer, led) {
            (Ok(_), true) => {
                FETCHED.fetch_add(1, Ordering::Relaxed);
                return answer;
            }
            (Ok(_), false) => {
                HITS.fetch_add(1, Ordering::Relaxed);
                return answer;
            }
            (Err(_), true) => return answer,
            (Err(_), false) => {}
        }
    }
}

/// [`lookup`] for a source that may be asked for many packages of one run
/// (Repology). A failure is kept for the rest of the process, so the same key
/// is not asked again; it is not kept across runs.
pub fn once_per_run(
    key: &str,
    fetch: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    ask_once(&FAILED, &ONCE_FLIGHTS, key, fetch)
}

/// Write the answers not yet on disk. Called when a sweep ends and when the
/// process exits; harmless when there is nothing to write.
pub fn flush() {
    if let Some(store) = STORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        store.save(now());
    }
}

#[cfg(test)]
#[path = "upstream_cache_tests.rs"]
mod tests;
