use super::*;

/// What the CLI's summaries need from a run, not every event. A long run emits
/// many events, and keeping them all only to add up a few fields grew without
/// bound. So this keeps running usage totals and the `PackageFinished` events.
pub struct MemorySink {
    pub(super) state: Mutex<MemoryState>,
}

#[derive(Default)]
pub(super) struct MemoryState {
    usage: Usage,
    finished: Vec<Event>,
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
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for MemorySink {
    fn report(&self, event: &Event) {
        match event {
            Event::AgentReply { usage, .. } | Event::NotesUsage { usage, .. } => {
                let mut st = self.lock();
                st.usage.input_tokens += usage.input_tokens;
                st.usage.output_tokens += usage.output_tokens;
                st.usage.cost_usd += usage.cost_usd;
            }
            Event::PackageFinished { .. } => self.lock().finished.push(event.clone()),
            _ => {}
        }
    }
}
