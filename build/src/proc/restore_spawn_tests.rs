use super::*;

#[test]
fn nothing_starts_after_shutdown_except_a_restore() {
    assert!(may_spawn(false, false));
    assert!(may_spawn(false, true));
    assert!(!may_spawn(true, false));
    assert!(may_spawn(true, true));
}

#[test]
fn a_shutdown_sweep_spares_the_restores_children() {
    let groups = std::collections::BTreeMap::from([(10, false), (11, true), (12, false)]);
    assert_eq!(sweep_targets(&groups), vec![10, 12]);
}

#[test]
fn the_wait_ends_when_the_last_restore_returns() {
    let counter = std::sync::atomic::AtomicUsize::new(0);
    assert!(wait_until_idle(&counter, Duration::ZERO, Duration::ZERO));
    counter.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(!wait_until_idle(
        &counter,
        Duration::from_millis(30),
        Duration::from_millis(5)
    ));
    let counter = std::sync::Arc::new(counter);
    let other = std::sync::Arc::clone(&counter);
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        other.store(0, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(wait_until_idle(
        &counter,
        Duration::from_secs(10),
        Duration::from_millis(5)
    ));
    t.join().unwrap();
}

#[test]
fn the_restore_permission_is_held_by_its_guard_only() {
    assert!(!restoring());
    {
        let _guard = restoring_after_shutdown();
        assert!(restoring());
    }
    assert!(!restoring());
}
