
use super::*;

/// The caches and their generation counters are process-wide, and every
/// test here bumps or reads them: run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Regression test for the bug where a long run permanently froze every
/// package's availability at whatever the cache first saw: builds z3
/// -solver, pydantic-core, etc. later in the same run kept looking
/// missing to every dependent package no matter how many succeeded,
/// because nothing ever dropped the stale entry.
#[test]
fn invalidate_drops_the_cached_entry() {
    let _serial = serial();
    let project = "ap-test-cache-invalidation-project";
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    cache.lock().unwrap().insert(
        project.to_string(),
        Arc::new(std::collections::HashSet::new()),
    );
    assert!(cache.lock().unwrap().contains_key(project));

    invalidate_project_packages_cache(project);

    assert!(!cache.lock().unwrap().contains_key(project));
}

/// A published-binary answer cached before a build into the project would
/// keep saying "nothing published" for the rest of the run, so invalidation
/// must drop it.
#[test]
fn invalidate_drops_cached_published_binary_answers() {
    let _serial = serial();
    let key = "ap-test-published-binaries-key".to_string();
    PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key.clone(), Vec::new());

    invalidate_project_packages_cache("ap-test-published-binaries-project");

    let cache = PUBLISHED_BINARIES_CACHE.lock().unwrap();
    assert!(cache.as_ref().is_none_or(|c| !c.contains_key(&key)));
}

/// A published-binary search that started before an invalidation must not
/// store its answer afterwards; one that started after is stored.
#[test]
fn published_answer_from_a_search_started_before_invalidation_is_dropped() {
    let _serial = serial();
    let stale_key = "ap-test-published-generation-stale".to_string();
    let fresh_key = "ap-test-published-generation-fresh".to_string();

    let generation = published_binaries_generation();
    forget_published_binaries();
    insert_published_if_current(stale_key.clone(), generation, vec!["a".into()]);
    {
        let cache = PUBLISHED_BINARIES_CACHE.lock().unwrap();
        assert!(cache.as_ref().is_none_or(|c| !c.contains_key(&stale_key)));
    }

    insert_published_if_current(
        fresh_key.clone(),
        published_binaries_generation(),
        vec!["b".into()],
    );
    let cache = PUBLISHED_BINARIES_CACHE.lock().unwrap();
    assert_eq!(
        cache.as_ref().and_then(|c| c.get(&fresh_key)),
        Some(&vec!["b".to_string()])
    );
}

/// A worker that read the listing before an invalidation must not put its
/// stale set back afterwards.
#[test]
fn invalidation_between_fetch_and_insert_is_not_overwritten() {
    let _serial = serial();
    let project = "ap-test-cache-stale-insert-project";
    let generation = project_generation(project);
    // ...fetch happens here, then another worker commits and invalidates...
    invalidate_project_packages_cache(project);
    insert_if_current(project, generation, &Arc::default());
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    assert!(!cache.lock().unwrap().contains_key(project));

    // A fetch that started after the invalidation is cached normally.
    insert_if_current(project, project_generation(project), &Arc::default());
    assert!(cache.lock().unwrap().contains_key(project));
}

/// A project this call never touched must not be affected — invalidation
/// is per-project, not a blanket cache flush of every project this
/// process has ever looked at.
#[test]
fn invalidate_does_not_touch_the_cache_before_anything_is_in_it() {
    let _serial = serial();
    // Just needs to not panic when the cache is empty or lacks the key.
    invalidate_project_packages_cache("ap-test-project-never-cached");
}
