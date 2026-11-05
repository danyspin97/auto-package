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
    let json = format!(r#"{{"version":"1","at":100,"logic":{LOGIC_VERSION}}}"#);
    let legacy: Entry = serde_json::from_str(&json).unwrap();
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
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "k",
        ttl,
        1000,
        false,
        || fetch("a"),
    );
    assert_eq!(got.as_deref(), Ok("a"));
    // Within the TTL: kept, no fetch.
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "k",
        ttl,
        1000 + 3599,
        false,
        || fetch("b"),
    );
    assert_eq!(got.as_deref(), Ok("a"));
    assert_eq!(asked.get(), 1);
    // Past the TTL: fetched again.
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "k",
        ttl,
        1000 + 3600,
        false,
        || fetch("c"),
    );
    assert_eq!(got.as_deref(), Ok("c"));
    // `refresh` fetches although the answer is fresh.
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "k",
        ttl,
        4000,
        true,
        || fetch("d"),
    );
    assert_eq!(got.as_deref(), Ok("d"));
    assert_eq!(asked.get(), 3);
    // A failed fetch is not kept: the next call asks again.
    let failing = || Err("503".to_string());
    assert_eq!(
        lookup_in(
            &cache,
            dir.path(),
            &InFlight::new(),
            "f",
            ttl,
            5000,
            false,
            failing
        ),
        Err("503".into())
    );
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "f",
        ttl,
        5001,
        false,
        || fetch("e"),
    );
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
fn fresh_reads_a_kept_answer_only_within_its_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Mutex::new(None);
    let ttl = Duration::from_secs(60);
    let got = lookup_in(
        &cache,
        dir.path(),
        &InFlight::new(),
        "k",
        ttl,
        1000,
        false,
        || Ok("a".to_string()),
    );
    assert_eq!(got.as_deref(), Ok("a"));
    assert_eq!(
        fresh_in(&cache, dir.path(), "k", 1059).as_deref(),
        Some("a")
    );
    assert_eq!(fresh_in(&cache, dir.path(), "k", 1060), None);
    assert_eq!(fresh_in(&cache, dir.path(), "other", 1000), None);
}

#[test]
fn a_joined_lookup_that_failed_is_asked_again_as_a_sequential_caller_would() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Mutex::new(None);
    let flights = InFlight::new();
    let attempts = AtomicUsize::new(0);
    let ttl = Duration::from_secs(3600);
    let answers: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    lookup_in(&cache, dir.path(), &flights, "k", ttl, 1000, false, || {
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            std::thread::sleep(Duration::from_millis(40));
                            Err("503".to_string())
                        } else {
                            Ok("2.0".to_string())
                        }
                    })
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // One caller got the failure, as the first of four sequential calls would;
    // the others got the answer of the retry.
    assert_eq!(answers.iter().filter(|a| a.is_err()).count(), 1);
    assert_eq!(answers.iter().filter(|a| a.is_ok()).count(), 3);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
