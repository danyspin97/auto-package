//! The builds of a project run: each job starts on its own thread as soon as
//! it is handed over, once the jobs it waits for have ended, at most `jobs`
//! at a time. With `jobs` 1 every job runs on the caller's thread when it is
//! handed over, which is the sequential run.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};

use crate::build_cmd::{Permit, Semaphore, make_semaphore, panic_message};

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
    fn run(&self, job: Self::Job) -> Self::Out;
    /// What `job` ends with when it is not run, or its run panicked: `why`.
    fn not_run(&self, job: Self::Job, why: String) -> Self::Out;
    /// Whether the run is stopping: a job that has not started is not run.
    fn stopping(&self) -> bool;
}

/// Set once a job has ended, however it ended.
#[derive(Default)]
struct Ended {
    ended: Mutex<bool>,
    cvar: Condvar,
}

impl Ended {
    fn wait(&self) {
        let mut ended = self.ended.lock().unwrap_or_else(|p| p.into_inner());
        while !*ended {
            ended = self.cvar.wait(ended).unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// Sets its `Ended` on drop, so a job that panics still releases its waiters.
struct SetOnDrop(Arc<Ended>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        *self.0.ended.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.cvar.notify_all();
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
            inline: jobs == 1,
            by_key: HashMap::new(),
            entries: Vec::new(),
        }
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
                State::Done(run_one(self.work, job))
            } else {
                let work = self.work;
                let gate = self.gate.clone();
                let set = SetOnDrop(ended.clone());
                State::Running(self.scope.spawn(move || {
                    let _set = set;
                    for w in &waits {
                        w.wait();
                    }
                    let _permit = Permit::acquire(&gate);
                    run_one(work, job)
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
