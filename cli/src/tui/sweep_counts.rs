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
    /// `checked 42 (7 to update, 30 current, 5 skipped)`, `checking` while the
    /// sweep goes on.
    pub(super) fn label(&self) -> String {
        format!(
            "{} {} ({} to update, {} current, {} skipped)",
            if self.finished {
                "checked"
            } else {
                "checking:"
            },
            self.checked,
            self.to_update,
            self.up_to_date,
            self.skipped
        )
    }

    /// `37 checked, 2 to update`: the part of [`Self::label`] a narrow header
    /// keeps.
    pub(super) fn compact_label(&self) -> String {
        format!("{} checked, {} to update", self.checked, self.to_update)
    }
}
