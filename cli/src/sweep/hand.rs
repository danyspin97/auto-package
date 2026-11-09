//! The gate between a sweep's thread and the run that takes the updates it
//! finds: see [`Hand`].

use super::*;

/// The sending end of a sweep's channel, behind a lock. A candidate is queued
/// and handed over only while the channel is open, under that lock, so once
/// [`Hand::close`] has run no row is queued for a candidate that is not in the
/// channel: what the channel still holds is the whole of what was found and
/// not taken.
pub(super) struct Hand(Mutex<Option<std::sync::mpsc::Sender<ap_build::update::Candidate>>>);

impl Hand {
    pub(super) fn new(tx: std::sync::mpsc::Sender<ap_build::update::Candidate>) -> Self {
        Hand(Mutex::new(Some(tx)))
    }

    /// Runs `queue` on `c` and hands it over. `false` once closed, or when the
    /// receiver is gone: the sweep stops.
    pub(super) fn hand_over(
        &self,
        c: ap_build::update::Candidate,
        queue: impl FnOnce(&ap_build::update::Candidate),
    ) -> bool {
        let open = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(tx) = open.as_ref() else {
            return false;
        };
        queue(&c);
        tx.send(c).is_ok()
    }

    /// Closes the channel: nothing more is queued or handed over, and the
    /// receiver sees the end once it has drained what is in it.
    pub(super) fn close(&self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
}

#[cfg(test)]
mod hand_tests {
    use super::Hand;
    use ap_build::update::{Candidate, Decision, SkipReason};

    fn candidate(name: &str) -> Candidate {
        Candidate {
            rpm_name: name.to_string(),
            decision: Decision::Skipped(SkipReason::NoSpec),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_closed_hand_over_queues_nothing_and_keeps_what_was_found() {
        let (tx, rx) = std::sync::mpsc::channel();
        let hand = Hand::new(tx);
        let mut queued = Vec::new();
        assert!(hand.hand_over(candidate("a"), |c| queued.push(c.rpm_name.clone())));
        assert!(hand.hand_over(candidate("b"), |c| queued.push(c.rpm_name.clone())));
        hand.close();
        assert!(!hand.hand_over(candidate("c"), |c| queued.push(c.rpm_name.clone())));
        assert_eq!(queued, ["a", "b"]);
        // What was handed over and not taken is still in the channel, and the
        // channel ends once it is drained.
        let left: Vec<String> = rx.try_iter().map(|c| c.rpm_name).collect();
        assert_eq!(left, ["a", "b"]);
        assert!(rx.recv().is_err());
    }
}
