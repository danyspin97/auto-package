//! The requests in flight for one key, shared by the callers that ask for it at
//! once. The first caller makes the request; the others wait and take its
//! answer, error included.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

/// The requests in flight, by key. Concurrent callers for one key share one
/// request: the first makes it, the others wait for its answer, error included.
pub(crate) struct InFlight {
    slots: Mutex<BTreeMap<String, Arc<Slot>>>,
}

/// The answer of one request in flight, once it is known.
#[derive(Default)]
struct Slot {
    done: Mutex<Option<Result<String, String>>>,
    ready: Condvar,
}

/// The caller that makes a request. Dropping it, normally or by a panic, gives
/// the waiting callers the answer (or that the request did not finish) and
/// takes the key out of the table.
struct Lead<'a> {
    flights: &'a InFlight,
    key: &'a str,
    slot: Arc<Slot>,
    answer: Option<Result<String, String>>,
}

impl InFlight {
    pub(crate) const fn new() -> InFlight {
        InFlight {
            slots: Mutex::new(BTreeMap::new()),
        }
    }

    /// The answer to `fetch` for `key`: `fetch` runs unless a request for `key`
    /// is in flight, in which case its answer is waited for. The flag is true
    /// when this call made the request. `fetch` runs without the table locked.
    pub(crate) fn run(
        &self,
        key: &str,
        fetch: impl FnOnce() -> Result<String, String>,
    ) -> (Result<String, String>, bool) {
        let (slot, leader) = {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            match slots.get(key) {
                Some(slot) => (slot.clone(), false),
                None => {
                    let slot = Arc::new(Slot::default());
                    slots.insert(key.to_string(), slot.clone());
                    (slot, true)
                }
            }
        };
        if leader {
            return self.lead(key, slot, fetch);
        }
        let mut done = slot.done.lock().unwrap_or_else(|e| e.into_inner());
        while done.is_none() {
            done = slot.ready.wait(done).unwrap_or_else(|e| e.into_inner());
        }
        (
            (*done)
                .clone()
                .unwrap_or_else(|| Err(DID_NOT_FINISH_TEXT.into())),
            false,
        )
    }

    fn lead(
        &self,
        key: &str,
        slot: Arc<Slot>,
        fetch: impl FnOnce() -> Result<String, String>,
    ) -> (Result<String, String>, bool) {
        let mut lead = Lead {
            flights: self,
            key,
            slot,
            answer: None,
        };
        let answer = fetch();
        lead.answer = Some(answer.clone());
        (answer, true)
    }
}

pub(crate) const DID_NOT_FINISH_TEXT: &str = "the request did not finish";
/// The answer of a request whose caller panicked while it was in flight.
pub(crate) const PANICKED_TEXT: &str = "the request panicked";

/// Whether an answer is one a waiter got because the request was abandoned,
/// not because upstream said no: the waiter asks again, as a sequential caller
/// would after a request that gave no answer.
pub(crate) fn abandoned(answer: &Result<String, String>) -> bool {
    matches!(answer, Err(e) if e == DID_NOT_FINISH_TEXT || e == PANICKED_TEXT)
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        let answer = self.answer.take().unwrap_or_else(|| {
            let text = if std::thread::panicking() {
                PANICKED_TEXT
            } else {
                DID_NOT_FINISH_TEXT
            };
            Err(text.to_string())
        });
        // The key leaves the table first: a caller that is woken by the answer
        // and asks again must find no request in flight, not this finished one.
        {
            let mut slots = self.flights.slots.lock().unwrap_or_else(|e| e.into_inner());
            if slots
                .get(self.key)
                .is_some_and(|s| Arc::ptr_eq(s, &self.slot))
            {
                slots.remove(self.key);
            }
        }
        *self.slot.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
        self.slot.ready.notify_all();
    }
}

/// Ask `fetch` for `key` unless `failed` already holds a failure for it. A new
/// failure is recorded there; an answer is not (the cache file keeps those).
/// A caller that finds a request for `key` already in flight in `flights`
/// waits for it and takes its answer.
pub(crate) fn ask_once(
    failed: &Mutex<BTreeMap<String, String>>,
    flights: &InFlight,
    key: &str,
    mut fetch: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    loop {
        let known = failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        if let Some(error) = known {
            return Err(error);
        }
        let (answer, led) = flights.run(key, || {
            let answer = fetch();
            if let Err(error) = &answer {
                failed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key.to_string(), error.clone());
            }
            answer
        });
        // A request that was abandoned recorded nothing: the next round asks again.
        if led || !abandoned(&answer) {
            return answer;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn concurrent_callers_share_one_answer() {
        let flights = InFlight::new();
        let asked = AtomicUsize::new(0);
        let answers: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..6)
                .map(|_| {
                    scope.spawn(|| {
                        flights.run("k", || {
                            asked.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            Ok("1.0".to_string())
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(answers.iter().all(|(a, _)| a == &Ok("1.0".to_string())));
        assert_eq!(answers.iter().filter(|(_, led)| *led).count(), 1);
    }

    #[test]
    fn a_failed_request_is_given_to_the_callers_waiting_for_it() {
        let flights = InFlight::new();
        let asked = AtomicUsize::new(0);
        let answers: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        flights.run("k", || {
                            asked.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            Err("unknown project".to_string())
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(
            answers
                .iter()
                .all(|(a, _)| a == &Err("unknown project".to_string()))
        );
    }

    #[test]
    fn a_panicking_request_releases_its_waiters() {
        let flights = InFlight::new();
        std::thread::scope(|scope| {
            let leader = scope.spawn(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    flights.run("k", || {
                        std::thread::sleep(Duration::from_millis(30));
                        panic!("boom")
                    })
                }))
            });
            std::thread::sleep(Duration::from_millis(5));
            let (answer, led) = flights.run("k", || Ok("late".to_string()));
            assert!(!led, "the second caller waited for the first");
            assert_eq!(answer, Err(PANICKED_TEXT.to_string()));
            assert!(leader.join().unwrap().is_err());
        });
        // The key is free again.
        assert_eq!(
            flights.run("k", || Ok("2.0".into())),
            (Ok("2.0".into()), true)
        );
    }

    #[test]
    fn a_failure_is_asked_once_per_run() {
        let failed = Mutex::new(BTreeMap::new());
        let asked = std::cell::Cell::new(0);
        let fetch = || {
            asked.set(asked.get() + 1);
            Err("unknown project".to_string())
        };
        assert_eq!(
            ask_once(&failed, &InFlight::new(), "repology:x", fetch),
            Err("unknown project".into())
        );
        let again = || -> Result<String, String> { unreachable!("asked again") };
        assert_eq!(
            ask_once(&failed, &InFlight::new(), "repology:x", again),
            Err("unknown project".into())
        );
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_failure_of_one_key_does_not_hold_another() {
        let failed = Mutex::new(BTreeMap::new());
        let _ = ask_once(&failed, &InFlight::new(), "repology:x", || {
            Err("no".to_string())
        });
        assert_eq!(
            ask_once(&failed, &InFlight::new(), "repology:y", || Ok(
                "1.0".to_string()
            )),
            Ok("1.0".into())
        );
    }

    #[test]
    fn an_answer_is_not_kept_here() {
        let failed = Mutex::new(BTreeMap::new());
        let asked = std::cell::Cell::new(0);
        for _ in 0..2 {
            let fetch = || {
                asked.set(asked.get() + 1);
                Ok("2.0".to_string())
            };
            assert_eq!(
                ask_once(&failed, &InFlight::new(), "repology:x", fetch),
                Ok("2.0".into())
            );
        }
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn a_waiter_of_a_panicked_request_asks_again() {
        let failed = Mutex::new(BTreeMap::new());
        let flights = InFlight::new();
        let asked = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let leader = scope.spawn(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ask_once(&failed, &flights, "k", || {
                        std::thread::sleep(Duration::from_millis(30));
                        panic!("boom")
                    })
                }))
            });
            std::thread::sleep(Duration::from_millis(5));
            let answer = ask_once(&failed, &flights, "k", || {
                asked.fetch_add(1, Ordering::SeqCst);
                Ok("late".to_string())
            });
            assert_eq!(answer, Ok("late".to_string()));
            assert!(leader.join().unwrap().is_err());
        });
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(
            failed.lock().unwrap().is_empty(),
            "a panic is not a failure"
        );
    }
}
