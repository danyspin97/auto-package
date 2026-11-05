use super::super::plan::skipped;
use super::super::{Decision, SkipReason};
use super::*;
use crate::upstream_flights::{InFlight, ask_once};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// A stand-in check: sleeps a name-dependent time, so answers finish out of
/// name order, and fails (no spec) for some names.
fn fake(name: &str) -> Candidate {
    let h = name
        .bytes()
        .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    std::thread::sleep(Duration::from_micros(h % 1500));
    if h % 5 == 0 {
        return skipped(name, SkipReason::SpecHeaderMissing);
    }
    Candidate {
        rpm_name: name.to_string(),
        decision: Decision::Skipped(SkipReason::SpecHeaderMissing),
        notes: vec![format!("{name}:{}", h % 97)],
    }
}

/// What a panicking check gives, as `plan_each` makes it.
fn panic_answer(name: &str, why: &str) -> Candidate {
    skipped(
        name,
        SkipReason::LookupFailed {
            ecosystem_tag: "check".into(),
            name: name.into(),
            error: format!("the check panicked: {why}"),
        },
    )
}

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("pkg{i:03}")).collect()
}

/// The answers handed on, and the progress calls, for `jobs`; `on` stops
/// after the answer at `stop_at`, when given.
fn run(
    names: &[String],
    jobs: usize,
    stop_at: Option<usize>,
    check: &(dyn Fn(&str) -> Candidate + Sync),
) -> (Vec<Candidate>, Vec<usize>) {
    let out = Mutex::new(Vec::new());
    let progressed = Mutex::new(Vec::new());
    check_in_order(
        names,
        jobs,
        check,
        &panic_answer,
        &|i| progressed.lock().unwrap().push(i),
        &mut |i, c| {
            out.lock().unwrap().push(c);
            stop_at != Some(i)
        },
    );
    (out.into_inner().unwrap(), progressed.into_inner().unwrap())
}

#[test]
fn a_pool_of_one_and_a_pool_of_eight_give_the_same_plan() {
    let names = names(200);
    let (one, one_progress) = run(&names, 1, None, &fake);
    let (eight, eight_progress) = run(&names, 8, None, &fake);
    assert_eq!(one.len(), 200);
    assert_eq!(one, eight);
    assert_eq!(one_progress, (0..200).collect::<Vec<_>>());
    assert_eq!(eight_progress, one_progress);
    assert!(one.iter().any(|c| c.notes.is_empty()), "some lookups fail");
}

#[test]
fn a_refusal_stops_both_pools_at_the_same_package() {
    let names = names(120);
    let (one, _) = run(&names, 1, Some(37), &fake);
    let (eight, _) = run(&names, 8, Some(37), &fake);
    assert_eq!(one.len(), 38);
    assert_eq!(one, eight);
    assert_eq!(eight.last().map(|c| c.rpm_name.as_str()), Some("pkg037"));
}

#[test]
fn more_jobs_than_names_and_no_names_are_fine() {
    let few = names(3);
    assert_eq!(run(&few, 8, None, &fake).0, run(&few, 1, None, &fake).0);
    assert!(run(&[], 8, None, &fake).0.is_empty());
}

#[test]
fn no_more_checks_run_at_once_than_jobs() {
    let now = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let counted = |name: &str| {
        let at = now.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(at, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(2));
        now.fetch_sub(1, Ordering::SeqCst);
        fake(name)
    };
    let (out, _) = run(&names(60), 3, None, &counted);
    assert_eq!(out.len(), 60);
    assert!(peak.load(Ordering::SeqCst) <= 3);
}

#[test]
fn a_panicking_check_is_one_failed_package_and_the_rest_go_on() {
    let check = |name: &str| {
        if name == "pkg003" || name == "pkg010" {
            panic!("boom in {name}");
        }
        fake(name)
    };
    let names = names(30);
    let (one, _) = run(&names, 1, None, &check);
    let (eight, _) = run(&names, 8, None, &check);
    assert_eq!(one.len(), 30);
    assert_eq!(one, eight);
    let failed = &eight[3];
    assert_eq!(failed.rpm_name, "pkg003");
    assert!(failed.notes.is_empty());
    assert_eq!(
        failed.decision,
        Decision::Skipped(SkipReason::LookupFailed {
            ecosystem_tag: "check".into(),
            name: "pkg003".into(),
            error: "the check panicked: boom in pkg003".into(),
        })
    );
    assert!(!eight[4].notes.is_empty(), "the next package is checked");
}

#[test]
fn a_slow_first_name_does_not_let_the_others_run_ahead() {
    let started = AtomicUsize::new(0);
    let started_when_first_done = AtomicUsize::new(0);
    let check = |name: &str| {
        started.fetch_add(1, Ordering::SeqCst);
        if name == "pkg000" {
            std::thread::sleep(Duration::from_millis(80));
            started_when_first_done.store(started.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        fake(name)
    };
    let (out, progressed) = run(&names(40), 4, None, &check);
    assert_eq!(out.len(), 40);
    assert_eq!(progressed, (0..40).collect::<Vec<_>>());
    // Only the names within a pool's width of the waiting answer may start.
    assert!(started_when_first_done.load(Ordering::SeqCst) <= 4);
}

/// A check that asks a shared key through `ask_once`, with the failed and
/// in-flight tables of one run; `asked` counts the requests the key made.
fn shared_key_check<'a>(
    failed: &'a Mutex<BTreeMap<String, String>>,
    flights: &'a InFlight,
    asked: &'a AtomicUsize,
) -> impl Fn(&str) -> Candidate + Sync + 'a {
    move |name: &str| {
        let answer = ask_once(failed, flights, "repology:flaky", || {
            asked.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            Err("upstream answered 503".to_string())
        });
        Candidate {
            rpm_name: name.to_string(),
            decision: Decision::Skipped(SkipReason::SpecHeaderMissing),
            notes: vec![format!("{answer:?}")],
        }
    }
}

#[test]
fn a_failing_key_shared_by_packages_is_asked_once_for_any_pool_size() {
    let names = names(24);
    let answers = |jobs: usize| {
        let (failed, flights, asked) = (
            Mutex::new(BTreeMap::new()),
            InFlight::new(),
            AtomicUsize::new(0),
        );
        let check = shared_key_check(&failed, &flights, &asked);
        let (out, _) = run(&names, jobs, None, &check);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "pool of {jobs}");
        out
    };
    let one = answers(1);
    assert_eq!(one.len(), 24);
    assert_eq!(one, answers(8));
}

/// Serialises the tests that set the environment variable.
static ENV: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn a_number_of_packages_is_read_from_the_text_and_a_bad_one_is_refused() {
    assert_eq!(parse_jobs(" 3 "), Some(3));
    assert_eq!(parse_jobs("1"), Some(1));
    assert_eq!(parse_jobs("0"), None);
    assert_eq!(parse_jobs("many"), None);
    assert_eq!(parse_jobs(""), None);
    assert_eq!(jobs_from_env(None), DEFAULT_JOBS);
    assert_eq!(jobs_from_env(Some("2")), 2);
    assert_eq!(jobs_from_env(Some("0")), DEFAULT_JOBS);
}

#[test]
fn the_pool_size_comes_from_the_flag_then_the_environment() {
    let _env = env_lock();
    let saved = std::env::var_os(JOBS_ENV);
    // SAFETY: `ENV` serialises the tests that change the variable, and no
    // other test reads it.
    unsafe { std::env::remove_var(JOBS_ENV) };
    set_check_jobs(0);
    assert_eq!(check_jobs(), DEFAULT_JOBS);
    unsafe { std::env::set_var(JOBS_ENV, "5") };
    assert_eq!(check_jobs(), 5);
    unsafe { std::env::set_var(JOBS_ENV, "0") };
    assert_eq!(check_jobs(), DEFAULT_JOBS);
    set_check_jobs(3);
    assert_eq!(check_jobs(), 3, "the flag wins over the environment");
    set_check_jobs(0);
    match saved {
        // SAFETY: as above.
        Some(v) => unsafe { std::env::set_var(JOBS_ENV, v) },
        None => unsafe { std::env::remove_var(JOBS_ENV) },
    }
}

#[test]
fn a_panic_on_the_main_side_does_not_hang_the_workers() {
    let (done, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let names = names(40);
        let mut seen = 0;
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            check_in_order(
                &names,
                8,
                &|n: &str| fake(n),
                &panic_answer,
                &|_| {},
                &mut |_, _| {
                    seen += 1;
                    if seen == 2 {
                        panic!("the consumer failed");
                    }
                    true
                },
            );
        }))
        .is_err();
        let _ = done.send(panicked);
    });
    match outcome.recv_timeout(Duration::from_secs(30)) {
        Ok(panicked) => assert!(panicked, "the consumer's panic reaches the caller"),
        Err(_) => panic!("check_in_order hung after its consumer panicked"),
    }
}
