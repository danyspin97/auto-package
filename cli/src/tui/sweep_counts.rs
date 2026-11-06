/// The counts of an `Event::SweepProgress`.
#[derive(Clone, Copy)]
pub(super) struct SweepCounts {
    pub(super) checked: usize,
    pub(super) to_update: usize,
    pub(super) up_to_date: usize,
    pub(super) skipped: usize,
    pub(super) finished: bool,
}

impl SweepCounts {
    /// `checked 42 · up-to-date 30 · needs update 7`, with `, checking…`
    /// after the count while the sweep goes on.
    pub(super) fn label(&self) -> String {
        format!(
            "checked {}{} · up-to-date {} · needs update {}",
            self.checked,
            self.in_flight(", checking…"),
            self.up_to_date,
            self.to_update
        )
    }

    /// `checked 42 · current 30 · update 7`: the part of [`Self::label`] a
    /// narrow header keeps.
    pub(super) fn compact_label(&self) -> String {
        format!(
            "checked {}{} · current {} · update {}",
            self.checked,
            self.in_flight("…"),
            self.up_to_date,
            self.to_update
        )
    }

    fn in_flight(&self, mark: &'static str) -> &'static str {
        if self.finished { "" } else { mark }
    }
}
