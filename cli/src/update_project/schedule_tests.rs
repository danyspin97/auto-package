use super::schedule::{POST_JOBS, Scheduler, Work, batch_order, interleave};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A job of the fake: its name, the names it waits for, and how long it runs.
#[derive(Clone)]
struct Job {
    name: &'static str,
    after: Vec<&'static str>,
    ms: u64,
}

fn job(name: &'static str, after: &[&'static str], ms: u64) -> Job {
    Job {
        name,
        after: after.to_vec(),
        ms,
    }
}

#[derive(Default)]
struct Fake {
    log: Mutex<Vec<String>>,
    running: AtomicUsize,
    most: AtomicUsize,
    stop: AtomicBool,
    threads: Mutex<Vec<(&'static str, std::thread::ThreadId)>>,
    /// Set by `release`: the post steps of the `held-` jobs wait for it.
    released: Mutex<bool>,
    released_cvar: Condvar,
}

impl Fake {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn at(&self, line: &str) -> usize {
        self.log().iter().position(|l| l == line).unwrap()
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.released_cvar.notify_all();
    }

    fn wait_released(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.released_cvar.wait(released).unwrap();
        }
    }
}

/// Waits for `line` to be logged, for a few seconds: false when it is not.
fn wait_for(fake: &Fake, line: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if fake.log().iter().any(|l| l == line) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

impl Work for Fake {
    type Job = Job;
    type Out = String;
    fn keys(job: &Job) -> Vec<String> {
        vec![job.name.to_string()]
    }
    fn after(job: &Job) -> Vec<String> {
        job.after.iter().map(|s| s.to_string()).collect()
    }
    fn queued(&self, job: &Job) {
        self.log
            .lock()
            .unwrap()
            .push(format!("queued {}", job.name));
    }
    fn run(&self, job: Job) -> String {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        self.log.lock().unwrap().push(format!("start {}", job.name));
        self.threads
            .lock()
            .unwrap()
            .push((job.name, std::thread::current().id()));
        std::thread::sleep(Duration::from_millis(job.ms));
        self.log.lock().unwrap().push(format!("end {}", job.name));
        self.running.fetch_sub(1, Ordering::SeqCst);
        if job.name.starts_with("panic") {
            panic!("boom in {}", job.name);
        }
        format!("built {}", job.name)
    }
    fn post(&self, out: String) -> String {
        // The name is the output's first word before any `:`, without `built`.
        let name = out
            .split(':')
            .next()
            .unwrap_or_default()
            .trim_start_matches("built ")
            .to_string();
        if !(name.starts_with("held-") || name.starts_with("post-panic")) {
            return out;
        }
        self.log.lock().unwrap().push(format!("post {name}"));
        if name.starts_with("held-") {
            self.wait_released();
        }
        if name.starts_with("post-panic") {
            panic!("boom in post {name}");
        }
        self.log.lock().unwrap().push(format!("post end {name}"));
        out
    }
    fn not_run(&self, job: Job, why: String) -> String {
        format!("{}: {why}", job.name)
    }
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Hand each batch over at anchor 0 and return the outputs.
fn run(fake: &Fake, jobs: usize, batches: Vec<Vec<Job>>) -> Vec<String> {
    std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, fake, jobs);
        for batch in batches {
            sched.submit(batch, 0);
        }
        sched.finish().into_iter().map(|(_, out)| out).collect()
    })
}

#[test]
fn outputs_come_in_the_order_handed_over_not_the_order_finished() {
    let fake = Fake::default();
    let out = run(
        &fake,
        0,
        vec![vec![job("slow", &[], 80)], vec![job("fast", &[], 0)]],
    );
    assert_eq!(out, vec!["built slow", "built fast"]);
    assert!(
        fake.at("end fast") < fake.at("end slow"),
        "{:?}",
        fake.log()
    );
}

#[test]
fn every_job_is_queued_when_handed_over_and_they_run_at_once() {
    let fake = Fake::default();
    std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 0);
        sched.submit(vec![job("a", &[], 50)], 0);
        sched.submit(vec![job("b", &[], 50)], 0);
        // Both are queued before either has ended.
        let log = fake.log();
        assert!(log.contains(&"queued a".to_string()));
        assert!(log.contains(&"queued b".to_string()));
        assert!(!log.iter().any(|l| l.starts_with("end")), "{log:?}");
        sched.finish();
    });
    assert_eq!(fake.most.load(Ordering::SeqCst), 2);
}

#[test]
fn a_dependency_ends_before_what_needs_it_starts() {
    let fake = Fake::default();
    // The dependent comes first in the batch, the dependency is slow, and a
    // dependency of the dependency comes last.
    let out = run(
        &fake,
        0,
        vec![vec![
            job("p", &["d"], 0),
            job("d", &["d2"], 40),
            job("d2", &[], 40),
        ]],
    );
    assert!(fake.at("end d2") < fake.at("start d"), "{:?}", fake.log());
    assert!(fake.at("end d") < fake.at("start p"), "{:?}", fake.log());
    assert_eq!(out, vec!["built d2", "built d", "built p"]);
}

#[test]
fn a_dependency_handed_over_in_an_earlier_batch_is_waited_for() {
    let fake = Fake::default();
    run(
        &fake,
        0,
        vec![
            vec![job("d", &[], 60), job("p", &["d"], 0)],
            vec![job("q", &["d"], 0)],
        ],
    );
    assert!(fake.at("end d") < fake.at("start q"), "{:?}", fake.log());
}

#[test]
fn jobs_bounds_how_many_run_at_once() {
    let fake = Fake::default();
    let batches = (0..6)
        .map(|i| vec![job(["a", "b", "c", "d", "e", "f"][i], &[], 30)])
        .collect();
    let out = run(&fake, 2, batches);
    assert_eq!(out.len(), 6);
    assert!(fake.most.load(Ordering::SeqCst) <= 2);
}

#[test]
fn with_one_job_a_batch_is_queued_whole_before_any_of_it_runs() {
    let fake = Fake::default();
    std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 1);
        sched.submit(
            vec![job("a", &[], 0), job("b", &[], 0), job("c", &[], 0)],
            0,
        );
        assert_eq!(
            fake.log(),
            vec![
                "queued a", "queued b", "queued c", "start a", "end a", "start b", "end b",
                "start c", "end c",
            ]
        );
        sched.finish();
    });
}

#[test]
fn one_job_runs_each_on_the_caller_in_turn() {
    let fake = Fake::default();
    let caller = std::thread::current().id();
    std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 1);
        sched.submit(vec![job("p", &["d"], 0), job("d", &[], 10)], 0);
        // Run before `submit` returned, the dependency first.
        assert_eq!(
            fake.log(),
            vec![
                "queued d", "queued p", "start d", "end d", "start p", "end p"
            ]
        );
        sched.submit(vec![job("q", &[], 0)], 0);
        assert_eq!(fake.log().last().unwrap(), "end q");
        sched.finish();
    });
    assert!(
        fake.threads
            .lock()
            .unwrap()
            .iter()
            .all(|(_, t)| *t == caller)
    );
    assert_eq!(fake.most.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panic_is_that_jobs_output_and_the_others_still_run() {
    for jobs in [0, 1] {
        let fake = Fake::default();
        let out = run(
            &fake,
            jobs,
            vec![vec![job("panic-a", &[], 0)], vec![job("b", &[], 0)]],
        );
        assert_eq!(
            out,
            vec!["panic-a: build panicked: boom in panic-a", "built b"]
        );
    }
}

#[test]
fn nothing_starts_once_the_run_is_stopping() {
    for jobs in [0, 1, 2] {
        let fake = Fake::default();
        fake.stop.store(true, Ordering::SeqCst);
        let out = run(
            &fake,
            jobs,
            vec![vec![job("a", &[], 0), job("b", &["a"], 0)]],
        );
        assert_eq!(
            out,
            vec![
                "a: interrupted before it started",
                "b: interrupted before it started"
            ]
        );
        assert!(!fake.log().iter().any(|l| l.starts_with("start")));
    }
}

fn names(v: &[&[&str]]) -> Vec<Vec<String>> {
    v.iter()
        .map(|n| n.iter().map(|s| s.to_string()).collect())
        .collect()
}

#[test]
fn a_batch_runs_dependencies_first_and_otherwise_in_the_order_given() {
    let keys = names(&[&["p"], &["d"], &["x"], &["d2"]]);
    let after = names(&[&["d"], &["d2"], &[], &[]]);
    assert_eq!(batch_order(&keys, &after), vec![2, 3, 1, 0]);
    // Waiting for a name outside the batch changes nothing.
    let after = names(&[&["elsewhere"], &[], &[], &[]]);
    assert_eq!(batch_order(&keys, &after), vec![0, 1, 2, 3]);
    // A cycle is broken at the first job of the two.
    let keys = names(&[&["a"], &["b"]]);
    let after = names(&[&["b"], &["a"]]);
    assert_eq!(batch_order(&keys, &after), vec![0, 1]);
}

#[test]
fn outputs_go_back_where_their_jobs_were_handed_over() {
    let own = vec!["skip1", "skip2", "skip3"];
    let built = vec![(1, vec!["b1", "b1dep"]), (1, vec!["b2"]), (3, vec!["b3"])];
    assert_eq!(
        interleave(own, built),
        vec!["skip1", "b1", "b1dep", "b2", "skip2", "skip3", "b3"]
    );
    assert_eq!(
        interleave(Vec::<&str>::new(), vec![(5, vec!["x"])]),
        vec!["x"]
    );
}

#[test]
fn a_post_step_does_not_hold_a_job_slot() {
    let fake = Fake::default();
    let (started, held, out) = std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 2);
        sched.submit(
            vec![
                job("held-a", &[], 0),
                job("held-b", &[], 0),
                job("c", &[], 0),
            ],
            0,
        );
        // Both slots are free again once the first two runs end, so `c` runs
        // while their post steps still wait.
        let started = wait_for(&fake, "start c");
        let held = fake.log().contains(&"post held-a".to_string())
            && fake.log().contains(&"post held-b".to_string())
            && !fake.log().contains(&"post end held-a".to_string());
        fake.release();
        (started, held, sched.finish())
    });
    assert!(started && held, "{:?}", fake.log());
    let out: Vec<String> = out.into_iter().map(|(_, o)| o).collect();
    assert_eq!(out, vec!["built held-a", "built held-b", "built c"]);
    assert!(fake.most.load(Ordering::SeqCst) <= 2);
}

#[test]
fn a_dependency_waits_for_the_run_of_its_dependency_not_the_post_step() {
    let fake = Fake::default();
    let (started, held) = std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 2);
        sched.submit(vec![job("held-d", &[], 0), job("p", &["held-d"], 0)], 0);
        let started = wait_for(&fake, "start p");
        let held = !fake.log().contains(&"post end held-d".to_string());
        fake.release();
        sched.finish();
        (started, held)
    });
    assert!(started && held, "{:?}", fake.log());
}

#[test]
fn results_stay_in_hand_over_order_when_post_steps_end_out_of_order() {
    let fake = Fake::default();
    let (b_done, out) = std::thread::scope(|s| {
        let mut sched = Scheduler::new(s, &fake, 2);
        sched.submit(vec![job("held-a", &[], 0), job("b", &[], 0)], 0);
        // `b` ends while the earlier `held-a` still waits in its post step.
        let b_done =
            wait_for(&fake, "end b") && !fake.log().contains(&"post end held-a".to_string());
        fake.release();
        (b_done, sched.finish())
    });
    assert!(b_done, "{:?}", fake.log());
    let out: Vec<String> = out.into_iter().map(|(_, o)| o).collect();
    assert_eq!(out, vec!["built held-a", "built b"]);
}

#[test]
fn post_steps_are_bounded_apart_from_the_run_slots() {
    let names = [
        "held-0", "held-1", "held-2", "held-3", "held-4", "held-5", "held-6", "held-7", "held-8",
        "held-9", "held-10", "held-11", "held-12", "held-13", "held-14", "held-15",
    ];
    let count = |fake: &Fake| {
        fake.log()
            .iter()
            .filter(|l| l.starts_with("post held-"))
            .count()
    };
    let fake = Fake::default();
    let total = POST_JOBS + 2;
    let batch: Vec<Job> = names[..total].iter().map(|n| job(*n, &[], 0)).collect();
    let (at_bound, out) = std::thread::scope(|s| {
        // No limit on the runs: only the post steps are bounded.
        let mut sched = Scheduler::new(s, &fake, 0);
        sched.submit(batch, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while count(&fake) < POST_JOBS && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(50));
        let at_bound = count(&fake);
        fake.release();
        (at_bound, sched.finish())
    });
    assert_eq!(at_bound, POST_JOBS, "{:?}", fake.log());
    assert_eq!(out.len(), total);
}

#[test]
fn a_panicking_post_step_is_that_jobs_failure_only() {
    for jobs in [1, 2] {
        let fake = Fake::default();
        let out = run(
            &fake,
            jobs,
            vec![vec![job("post-panic-a", &[], 0), job("b", &[], 0)]],
        );
        assert_eq!(
            out,
            vec![
                "post-panic-a: post-build step panicked: boom in post post-panic-a",
                "built b"
            ]
        );
    }
}
