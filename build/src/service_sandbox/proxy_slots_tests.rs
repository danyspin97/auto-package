//! The proxy's connection slots: at most `MAX_CONNECTIONS` are taken at once,
//! and a slot comes back when it is dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::proxy::{MAX_CONNECTIONS, take_slot};

#[test]
fn the_proxy_serves_at_most_sixteen_connections_at_once() {
    let taken = Arc::new(AtomicUsize::new(0));
    let held: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| take_slot(&taken, MAX_CONNECTIONS).expect("a slot"))
        .collect();
    assert!(take_slot(&taken, MAX_CONNECTIONS).is_none());
    assert!(take_slot(&taken, MAX_CONNECTIONS).is_none());
    assert_eq!(taken.load(Ordering::SeqCst), MAX_CONNECTIONS);
    drop(held);
    assert_eq!(taken.load(Ordering::SeqCst), 0);
    assert!(take_slot(&taken, MAX_CONNECTIONS).is_some());
    assert_eq!(taken.load(Ordering::SeqCst), 0);
}
