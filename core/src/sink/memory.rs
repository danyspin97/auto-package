use std::collections::HashSet;

use crate::event::{StepId, StepKind};

use super::*;

/// What the CLI's summaries need from a run, not every event. A long run emits
/// many events, and keeping them all only to add up a few fields grew without
/// bound. So this keeps running usage totals, the `PackageFinished` events and
/// the events of the build steps (see [`MemorySink::tries_records`]).
pub struct MemorySink {
    pub(super) state: Mutex<MemoryState>,
}

#[derive(Default)]
pub(super) struct MemoryState {
    usage: Usage,
    finished: Vec<Event>,
    unchecked: Vec<Event>,
    /// The build and failure-extract steps started so far, by package and
    /// step id, so the events of those steps alone are kept.
    tracked: HashSet<(String, u32)>,
    tries: Vec<(String, Event)>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MemoryState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tokens and cost of every agent reply and notes turn so far.
    pub fn usage(&self) -> Usage {
        self.lock().usage.clone()
    }

    /// The `PackageFinished` events so far, in order.
    pub fn finished(&self) -> Vec<Event> {
        self.lock().finished.clone()
    }

    /// The `VersionUnchecked` events so far, in order.
    pub fn version_unchecked(&self) -> Vec<Event> {
        self.lock().unchecked.clone()
    }

    /// The events the tries of each package are read from, in order, each
    /// with its package: the build steps' starts, ends and failures, the
    /// failure extracts of those steps, and the `PackageFinished` events.
    pub fn tries_records(&self) -> Vec<(String, Event)> {
        self.lock().tries.clone()
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for MemorySink {
    fn report(&self, event: &Event) {
        match event {
            Event::AgentReply { usage, .. }
            | Event::NotesUsage { usage, .. }
            | Event::FailedTurnUsage { usage, .. } => {
                let mut st = self.lock();
                st.usage.input_tokens += usage.input_tokens;
                st.usage.output_tokens += usage.output_tokens;
                st.usage.cost_usd += usage.cost_usd;
            }
            Event::PackageFinished { pkg, .. } => {
                let mut st = self.lock();
                st.finished.push(event.clone());
                st.tries.push((pkg.clone(), event.clone()));
            }
            Event::VersionUnchecked { .. } => self.lock().unchecked.push(event.clone()),
            _ => self.keep_try_event(event),
        }
    }
}

impl MemorySink {
    /// Keeps the events of the build and extract steps, which are tagged with
    /// the package of the ambient [`current_pkg`].
    fn keep_try_event(&self, event: &Event) {
        let Some(pkg) = current_pkg() else {
            return;
        };
        let mut st = self.lock();
        let key = |step: &StepId| (pkg.clone(), step.0);
        match event {
            Event::StepStarted {
                step,
                kind: StepKind::Build | StepKind::ExtractFailure,
                ..
            } => {
                st.tracked.insert(key(step));
                st.tries.push((pkg.clone(), event.clone()));
            }
            Event::StepFinished { step, .. }
            | Event::BuildFailed { step, .. }
            | Event::ExtractResult { step, .. }
                if st.tracked.contains(&key(step)) =>
            {
                st.tries.push((pkg.clone(), event.clone()));
            }
            _ => {}
        }
    }
}
