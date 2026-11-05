//! At most [`PER_HOST`] upstream requests in flight to one host, whatever the
//! size of the check pool. The per-host rate limit (`ecosystem::net`) spaces
//! requests out; this bounds how many are waiting on a host at once.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};

/// Requests in flight to one host.
pub const PER_HOST: usize = 4;

/// Counts of requests in flight, by host.
pub(super) struct Gate {
    busy: Mutex<BTreeMap<String, usize>>,
    freed: Condvar,
    limit: usize,
}

/// A request in flight to a host; the slot is given back on drop.
pub(super) struct Permit<'a> {
    gate: &'a Gate,
    host: String,
}

impl Gate {
    pub(super) const fn new(limit: usize) -> Gate {
        Gate {
            busy: Mutex::new(BTreeMap::new()),
            freed: Condvar::new(),
            limit,
        }
    }

    /// Wait until `host` has a free slot, then take it.
    pub(super) fn enter<'a>(&'a self, host: &str) -> Permit<'a> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while busy.get(host).copied().unwrap_or(0) >= self.limit {
            busy = self.freed.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy.entry(host.to_string()).or_insert(0) += 1;
        Permit {
            gate: self,
            host: host.to_string(),
        }
    }

    /// Requests in flight to `host` now.
    #[cfg(test)]
    pub(super) fn in_flight(&self, host: &str) -> usize {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(host)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut busy = self.gate.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = busy.get_mut(&self.host) {
            *n -= 1;
            if *n == 0 {
                busy.remove(&self.host);
            }
        }
        self.gate.freed.notify_all();
    }
}

static GATE: Gate = Gate::new(PER_HOST);

/// Wait for a slot on `host` in the run's gate.
pub(super) fn enter(host: &str) -> Permit<'static> {
    GATE.enter(host)
}

/// The host of an upstream URL (`https://host/path` gives `host`; a URL with
/// no scheme gives its first component), as the gate's key. Two URLs with one
/// key share its slots.
pub(super) fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next().unwrap_or(host);
    // A forge's API and its files are asked on other hosts than its web pages:
    // they share the page host's slots, so one forge gets one budget.
    match host {
        "api.github.com" => "github.com",
        "files.pythonhosted.org" => "pypi.org",
        _ => host,
    }
}

#[cfg(test)]
#[path = "host_gate_tests.rs"]
mod tests;
