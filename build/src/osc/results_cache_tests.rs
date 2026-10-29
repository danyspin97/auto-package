
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const LONG: Duration = Duration::from_secs(60);

/// A one-shot gate. A fetch parks on it, so the test decides when an
/// in-flight fetch lands instead of sleeping and hoping it is in flight.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = self.cv.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }
}

fn key(project: &str) -> ResultsKey {
    (
        project.to_string(),
        "repo".to_string(),
        "x86_64".to_string(),
    )
}

fn doc(body: &str) -> Result<String, ApiFailure> {
    Ok(body.to_string())
}

#[test]
fn concurrent_waiters_trigger_one_fetch() {
    let cache = ResultsCache::new();
    let fetches = AtomicUsize::new(0);
    let gate = Gate::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    cache.get(&key("p"), LONG, None, || {
                        fetches.fetch_add(1, Ordering::SeqCst);
                        gate.wait();
                        doc("<resultlist/>")
                    })
                })
            })
            .collect();
        // The one fetch lands only once all the waiters have been started.
        gate.release();
        for h in handles {
            assert_eq!(&*h.join().unwrap().ok().unwrap(), "<resultlist/>");
        }
    });
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

#[test]
fn a_document_is_shared_until_it_expires() {
    let cache = ResultsCache::new();
    let fetches = AtomicUsize::new(0);
    let fetch = || {
        fetches.fetch_add(1, Ordering::SeqCst);
        doc("d")
    };
    for _ in 0..3 {
        cache.get(&key("p"), LONG, None, fetch).ok().unwrap();
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    // With no lifetime every read is a new fetch.
    cache
        .get(&key("p"), Duration::ZERO, None, fetch)
        .ok()
        .unwrap();
    cache
        .get(&key("p"), Duration::ZERO, None, fetch)
        .ok()
        .unwrap();
    assert_eq!(fetches.load(Ordering::SeqCst), 3);
}

#[test]
fn keys_do_not_share_documents() {
    let cache = ResultsCache::new();
    let a = cache.get(&key("a"), LONG, None, || doc("a")).ok().unwrap();
    let b = cache.get(&key("b"), LONG, None, || doc("b")).ok().unwrap();
    let mut other_arch = key("a");
    other_arch.2 = "aarch64".to_string();
    let c = cache
        .get(&other_arch, LONG, None, || doc("c"))
        .ok()
        .unwrap();
    assert_eq!((&*a, &*b, &*c), ("a", "b", "c"));
}

#[test]
fn a_fresh_read_skips_a_document_fetched_before_it() {
    let cache = ResultsCache::new();
    cache
        .get(&key("p"), LONG, None, || doc("old"))
        .ok()
        .unwrap();
    let committed = Instant::now();
    let fresh = cache
        .get(&key("p"), LONG, Some(committed), || doc("new"))
        .ok()
        .unwrap();
    assert_eq!(&*fresh, "new");
    // That document is itself fresh enough for the next such waiter.
    let again = cache
        .get(&key("p"), LONG, Some(committed), || doc("never"))
        .ok()
        .unwrap();
    assert_eq!(&*again, "new");
}

#[test]
fn a_fresh_read_waits_out_an_older_fetch_and_then_fetches_itself() {
    let cache = ResultsCache::new();
    let fetches = AtomicUsize::new(0);
    let started = Gate::default();
    let release = Gate::default();
    std::thread::scope(|scope| {
        let slow = scope.spawn(|| {
            cache.get(&key("p"), LONG, None, || {
                fetches.fetch_add(1, Ordering::SeqCst);
                started.release();
                release.wait();
                doc("before the commit")
            })
        });
        started.wait();
        let committed = Instant::now();
        // The old fetch lands before or after the read below asks for its
        // document; either way the read must not be given it.
        release.release();
        let fresh = cache
            .get(&key("p"), LONG, Some(committed), || {
                fetches.fetch_add(1, Ordering::SeqCst);
                doc("after the commit")
            })
            .ok()
            .unwrap();
        assert_eq!(&*fresh, "after the commit");
        assert_eq!(&*slow.join().unwrap().ok().unwrap(), "before the commit");
    });
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
}

#[test]
fn invalidation_drops_the_documents_of_one_project() {
    let cache = ResultsCache::new();
    cache.get(&key("p"), LONG, None, || doc("p1")).ok().unwrap();
    cache.get(&key("q"), LONG, None, || doc("q1")).ok().unwrap();
    cache.invalidate("p");
    let p = cache.get(&key("p"), LONG, None, || doc("p2")).ok().unwrap();
    let q = cache.get(&key("q"), LONG, None, || doc("q2")).ok().unwrap();
    assert_eq!((&*p, &*q), ("p2", "q1"));
}

#[test]
fn a_fetch_in_flight_during_an_invalidation_is_not_stored() {
    let cache = ResultsCache::new();
    let first = cache
        .get(&key("p"), LONG, None, || {
            cache.invalidate("p");
            doc("pre-commit")
        })
        .ok()
        .unwrap();
    // The fetcher still gets what it fetched, but nobody else does.
    assert_eq!(&*first, "pre-commit");
    let next = cache
        .get(&key("p"), LONG, None, || doc("post-commit"))
        .ok()
        .unwrap();
    assert_eq!(&*next, "post-commit");
}

#[test]
fn a_failure_is_shared_briefly_and_not_kept() {
    let cache = ResultsCache::new();
    let fetches = AtomicUsize::new(0);
    let failing = || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Err(ApiFailure::Http(503, "down".to_string()))
    };
    for _ in 0..3 {
        assert!(matches!(
            cache.get(&key("p"), LONG, None, failing),
            Err(ApiFailure::Http(503, _))
        ));
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    // Once the (short) error lifetime is over, it is asked again.
    let again = cache
        .get(&key("p"), Duration::ZERO, None, || doc("up"))
        .ok()
        .unwrap();
    assert_eq!(&*again, "up");
}

#[test]
fn a_fetch_that_panics_does_not_block_the_next_one() {
    let cache = ResultsCache::new();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = cache.get(&key("p"), LONG, None, || panic!("osc exploded"));
    }));
    assert!(crashed.is_err());
    let next = cache
        .get(&key("p"), LONG, None, || doc("fine"))
        .ok()
        .unwrap();
    assert_eq!(&*next, "fine");
}

const PROJECT_DOC: &str = r#"<resultlist>
  <result project="p" repository="repo" arch="x86_64" dirty="true">
    <status package="foo" code="building"/>
    <status package="foo:mini" code="succeeded"/>
    <status package="bar" code="blocked"><details>waiting for foo</details></status>
  </result>
</resultlist>"#;

fn entries_of(
    cache: &ResultsCache,
    pkg: &str,
    project: &AtomicUsize,
    single: &AtomicUsize,
    project_doc: &'static str,
    package_doc: &'static str,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    package_entries(
        cache,
        "p",
        "repo",
        "x86_64",
        pkg,
        None,
        || {
            project.fetch_add(1, Ordering::SeqCst);
            doc(project_doc)
        },
        || {
            single.fetch_add(1, Ordering::SeqCst);
            doc(package_doc)
        },
    )
}

#[test]
fn every_package_reads_its_entries_from_the_one_project_document() {
    let cache = ResultsCache::new();
    let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let foo = entries_of(&cache, "foo", &project, &single, PROJECT_DOC, "")
        .ok()
        .unwrap()
        .unwrap();
    let bar = entries_of(&cache, "bar", &project, &single, PROJECT_DOC, "")
        .ok()
        .unwrap()
        .unwrap();
    assert_eq!(project.load(Ordering::SeqCst), 1);
    assert_eq!(single.load(Ordering::SeqCst), 0);
    // Multibuild flavors come with the package, as with a per-package read.
    let flavors = obs_status::flavors_in(&foo, "repo", "x86_64", "foo");
    assert_eq!(flavors.len(), 1);
    assert_eq!(flavors[0].pkg, "foo:mini");
    let entry = obs_status::entry_in(&bar, "repo", "x86_64", "bar").unwrap();
    assert_eq!(entry.details.as_deref(), Some("waiting for foo"));
    assert!(entry.dirty);
}

#[test]
fn a_package_missing_from_the_shared_document_is_read_on_its_own() {
    let cache = ResultsCache::new();
    let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let own = r#"<resultlist><result project="p" repository="repo" arch="x86_64">
            <status package="new" code="scheduled"/></result></resultlist>"#;
    let entries = entries_of(&cache, "new", &project, &single, PROJECT_DOC, own)
        .ok()
        .unwrap()
        .unwrap();
    assert_eq!(single.load(Ordering::SeqCst), 1);
    assert_eq!(entries[0].code, "scheduled");
    // Missing there too: no entry, so the caller reads it as no result.
    let none = entries_of(
        &cache,
        "ghost",
        &project,
        &single,
        PROJECT_DOC,
        "<resultlist/>",
    );
    assert!(none.ok().unwrap().is_none());
}

#[test]
fn a_cut_off_shared_document_is_not_trusted() {
    let cache = ResultsCache::new();
    let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let cut = &PROJECT_DOC[..PROJECT_DOC.len() - 30];
    let own = r#"<resultlist><result project="p" repository="repo" arch="x86_64">
            <status package="bar" code="failed"/></result></resultlist>"#;
    let entries = entries_of(&cache, "bar", &project, &single, cut, own)
        .ok()
        .unwrap()
        .unwrap();
    assert_eq!(entries[0].code, "failed");
    assert_eq!(single.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refusal_of_the_shared_fetch_is_passed_on_without_another_request() {
    let cache = ResultsCache::new();
    let single = AtomicUsize::new(0);
    let result = package_entries(
        &cache,
        "p",
        "repo",
        "x86_64",
        "foo",
        None,
        || Err(ApiFailure::Http(429, "slow down".to_string())),
        || {
            single.fetch_add(1, Ordering::SeqCst);
            doc("")
        },
    );
    assert!(matches!(result, Err(ApiFailure::Http(429, _))));
    assert_eq!(single.load(Ordering::SeqCst), 0);
}
