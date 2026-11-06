//! The builds of a project run: each job starts on its own thread as soon as
//! it is handed over, once the jobs it waits for have ended, at most `jobs`
//! at a time. A job has two parts: the run, which holds one of the `jobs`
//! slots, and the post step, which runs after the slot is released, on a
//! separate pool of at most [`POST_JOBS`] threads. A job counts as ended when
//! its run ends, so the jobs that wait for it do not wait for its post step.
//! With `jobs` 1 every job runs on the caller's thread when it is handed over,
//! run and post step in turn, which is the sequential run.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};

use crate::build_cmd::{Permit, Semaphore, make_semaphore, panic_message};

/// How many post steps run at once, whatever `jobs` is: they wait on other
/// machines, so they do not take the slots the runs use.
pub(super) const POST_JOBS: usize = 8;

/// What the scheduler runs: the builds of a run, or a fake in the tests.
pub(super) trait Work: Sync {
    type Job: Clone + Send;
    type Out: Send;
    /// The names `job` is known by, which other jobs wait for.
    fn keys(job: &Self::Job) -> Vec<String>;
    /// The names of the jobs that must end before `job` starts.
    fn after(job: &Self::Job) -> Vec<String>;
    /// Told on the caller's thread when `job` is handed over.
    fn queued(&self, job: &Self::Job);
    /// The run of `job`: it holds one of the `jobs` slots.
    fn run(&self, job: Self::Job) -> Self::Out;
    /// What `job` ends with when it is not run, or its run panicked: `why`.
    fn not_run(&self, job: Self::Job, why: String) -> Self::Out;
    /// The post step of `job`, given what its run ended with. It runs once the
    /// slot is released and the jobs waiting for the run have started, so it
    /// may take long. Default: nothing to do.
    fn post(&self, out: Self::Out) -> Self::Out {
        out
    }
    /// Whether the run is stopping: a job that has not started is not run.
    fn stopping(&self) -> bool;
    /// Told once per job handed over when its run has ended, however it
    /// ended (not run, panicked), before the jobs waiting for it start.
    /// Default: nothing to do.
    fn run_ended(&self) {}
}

/// Set once a job's run has ended, however it ended.
#[derive(Default)]
struct Ended {
    ended: Mutex<bool>,
    cvar: Condvar,
}

impl Ended {
    fn set(&self) {
        *self.ended.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.cvar.notify_all();
    }

    fn wait(&self) {
        let mut ended = self.ended.lock().unwrap_or_else(|p| p.into_inner());
        while !*ended {
            ended = self.cvar.wait(ended).unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// Tells the work its job's run ended on drop, so a thread that panics
/// still tells it.
struct RunEndedOnDrop<'w, W: Work>(&'w W);

impl<W: Work> Drop for RunEndedOnDrop<'_, W> {
    fn drop(&mut self) {
        self.0.run_ended();
    }
}

/// Sets its `Ended` on drop, so a job that panics still releases its waiters.
struct SetOnDrop(Arc<Ended>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}

enum State<'scope, O> {
    Running(ScopedJoinHandle<'scope, O>),
    Done(O),
}

struct Entry<'scope, J, O> {
    anchor: usize,
    state: State<'scope, O>,
    ended: Arc<Ended>,
    /// What a thread that panicked past `not_run` ends with.
    fallback: J,
}

pub(super) struct Scheduler<'scope, 'env, W: Work> {
    scope: &'scope Scope<'scope, 'env>,
    work: &'env W,
    gate: Option<Semaphore>,
    post_gate: Option<Semaphore>,
    inline: bool,
    by_key: HashMap<String, usize>,
    entries: Vec<Entry<'scope, W::Job, W::Out>>,
}

impl<'scope, 'env, W> Scheduler<'scope, 'env, W>
where
    W: Work,
    W::Job: 'scope,
    W::Out: 'scope,
{
    /// `jobs` jobs run at once; 0 is no limit, 1 runs each on the caller.
    pub(super) fn new(scope: &'scope Scope<'scope, 'env>, work: &'env W, jobs: usize) -> Self {
        Scheduler {
            scope,
            work,
            gate: make_semaphore(jobs),
            post_gate: make_semaphore(POST_JOBS),
            inline: jobs == 1,
            by_key: HashMap::new(),
            entries: Vec::new(),
        }
    }

    /// What runs the jobs.
    pub(super) fn work(&self) -> &'env W {
        self.work
    }

    /// Hand over `batch`. `anchor` is where its outputs go among the caller's
    /// own (see [`interleave`]).
    pub(super) fn submit(&mut self, batch: Vec<W::Job>, anchor: usize) {
        let keys: Vec<Vec<String>> = batch.iter().map(W::keys).collect();
        let after: Vec<Vec<String>> = batch.iter().map(W::after).collect();
        let order = batch_order(&keys, &after);
        let mut slots: Vec<Option<W::Job>> = batch.into_iter().map(Some).collect();
        let ordered: Vec<(usize, W::Job)> = order
            .into_iter()
            .filter_map(|i| slots[i].take().map(|job| (i, job)))
            .collect();
        // The whole batch is queued before any of it runs, so it shows at once
        // even when each job runs on the caller (`jobs` 1).
        for (_, job) in &ordered {
            self.work.queued(job);
        }
        for (i, job) in ordered {
            // Only jobs handed over before this one are waited for, so no two
            // jobs wait for each other.
            let mut waits: Vec<Arc<Ended>> = Vec::new();
            for key in &after[i] {
                if let Some(&e) = self.by_key.get(key) {
                    let ended = &self.entries[e].ended;
                    if !waits.iter().any(|w| Arc::ptr_eq(w, ended)) {
                        waits.push(ended.clone());
                    }
                }
            }
            let index = self.entries.len();
            for key in &keys[i] {
                self.by_key.insert(key.clone(), index);
            }
            let ended = Arc::new(Ended::default());
            let fallback = job.clone();
            let state = if self.inline {
                let _set = SetOnDrop(ended.clone());
                let told = RunEndedOnDrop(self.work);
                let copy = job.clone();
                let out = run_one(self.work, job);
                drop(told);
                ended.set();
                State::Done(post_one(self.work, copy, out))
            } else {
                let work = self.work;
                let gate = self.gate.clone();
                let post_gate = self.post_gate.clone();
                let set = SetOnDrop(ended.clone());
                let early = ended.clone();
                State::Running(self.scope.spawn(move || {
                    let _set = set;
                    let told = RunEndedOnDrop(work);
                    for w in &waits {
                        w.wait();
                    }
                    let copy = job.clone();
                    let out = {
                        let _permit = Permit::acquire(&gate);
                        run_one(work, job)
                    };
                    // The run is over: the slot is free and the jobs that
                    // wait for this one may start, while the post step waits
                    // for its own bound.
                    drop(told);
                    early.set();
                    let _post = Permit::acquire(&post_gate);
                    post_one(work, copy, out)
                }))
            };
            self.entries.push(Entry {
                anchor,
                state,
                ended,
                fallback,
            });
        }
    }

    /// Wait for every job; their outputs with their anchors, in the order
    /// the jobs were handed over.
    pub(super) fn finish(self) -> Vec<(usize, W::Out)> {
        let work = self.work;
        self.entries
            .into_iter()
            .map(|e| {
                let out = match e.state {
                    State::Done(out) => out,
                    State::Running(handle) => handle.join().unwrap_or_else(|p| {
                        work.not_run(
                            e.fallback,
                            format!("build panicked: {}", panic_message(p.as_ref())),
                        )
                    }),
                };
                (e.anchor, out)
            })
            .collect()
    }
}

fn run_one<W: Work>(work: &W, job: W::Job) -> W::Out {
    if work.stopping() {
        return work.not_run(job, "interrupted before it started".to_string());
    }
    let copy = job.clone();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.run(job))).unwrap_or_else(|p| {
        work.not_run(
            copy,
            format!("build panicked: {}", panic_message(p.as_ref())),
        )
    })
}

/// The post step of `job`; a panic in it is that job's failure.
fn post_one<W: Work>(work: &W, job: W::Job, out: W::Out) -> W::Out {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.post(out))).unwrap_or_else(|p| {
        work.not_run(
            job,
            format!("post-build step panicked: {}", panic_message(p.as_ref())),
        )
    })
}

/// The order a batch runs in: each job after the jobs of the batch it waits
/// for, otherwise in the order given. A cycle is broken at the first job
/// left in the order given.
pub(super) fn batch_order(keys: &[Vec<String>], after: &[Vec<String>]) -> Vec<usize> {
    let n = keys.len();
    let needs = |i: usize, j: usize| i != j && after[i].iter().any(|k| keys[j].contains(k));
    let mut placed = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let free = (0..n).find(|&i| !placed[i] && (0..n).all(|j| placed[j] || !needs(i, j)));
        let next = free.unwrap_or_else(|| (0..n).find(|&i| !placed[i]).unwrap_or(0));
        placed[next] = true;
        order.push(next);
    }
    order
}

/// The caller's own items `own`, with each output of `built` put before the
/// item at its anchor (an anchor past the end goes last), the outputs in the
/// order given.
pub(super) fn interleave<T>(own: Vec<T>, mut built: Vec<(usize, Vec<T>)>) -> Vec<T> {
    built.sort_by_key(|(anchor, _)| *anchor);
    let mut built = built.into_iter().peekable();
    let mut out = Vec::new();
    for (i, item) in own.into_iter().enumerate() {
        while let Some((_, items)) = built.next_if(|(anchor, _)| *anchor <= i) {
            out.extend(items);
        }
        out.push(item);
    }
    for (_, items) in built {
        out.extend(items);
    }
    out
}
