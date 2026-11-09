/// The counts of an `Event::SweepProgress`.
#[derive(Clone, Copy)]
pub(super) struct SweepCounts {
    pub(super) checked: usize,
    /// How many packages the sweep checks in all, once known.
    pub(super) total: Option<usize>,
    pub(super) to_update: usize,
    pub(super) up_to_date: usize,
    pub(super) finished: bool,
}

impl SweepCounts {
    /// `checked 42/120 · up-to-date 30 · needs update 7`, with `, checking 3`
    /// after the count while the sweep goes on. `checking` is the number of
    /// checks begun and not yet answered.
    pub(super) fn label(&self, checking: usize) -> String {
        format!(
            "{}{} · up-to-date {} · needs update {}",
            self.checked_text(),
            self.checking_part(checking),
            self.up_to_date,
            self.to_update
        )
    }

    /// `checked 42/120 · current 30 · update 7`: the part of [`Self::label`] a
    /// narrow header keeps.
    pub(super) fn compact_label(&self, checking: usize) -> String {
        format!(
            "{}{} · current {} · update {}",
            self.checked_text(),
            self.checking_part(checking),
            self.up_to_date,
            self.to_update
        )
    }

    /// `checked 42`, or `checked 42/120` once the total is known.
    fn checked_text(&self) -> String {
        match self.total {
            Some(total) => format!("checked {}/{total}", self.checked),
            None => format!("checked {}", self.checked),
        }
    }

    /// `, checking 3` while the sweep goes on; nothing once it is finished.
    fn checking_part(&self, checking: usize) -> String {
        if self.finished {
            String::new()
        } else {
            format!(", checking {checking}")
        }
    }
}
