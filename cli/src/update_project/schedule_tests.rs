use super::schedule::{Scheduler, Work, batch_order, interleave};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

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
}

impl Fake {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn at(&self, line: &str) -> usize {
        self.log().iter().position(|l| l == line).unwrap()
    }
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
