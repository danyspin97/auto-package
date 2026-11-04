use super::{restore_if_taken, restore_sequence};
use std::cell::Cell;
use std::sync::atomic::AtomicBool;

#[test]
fn a_guard_for_a_tui_that_never_started_restores_nothing() {
    let taken = AtomicBool::new(false);
    let restored = Cell::new(false);
    restore_if_taken(&taken, || restored.set(true));
    assert!(!restored.get());
}

#[test]
fn a_guard_for_a_running_tui_restores_the_terminal() {
    let taken = AtomicBool::new(true);
    let restored = Cell::new(false);
    restore_if_taken(&taken, || restored.set(true));
    assert!(restored.get());
}

#[test]
fn the_restore_leaves_the_alternate_screen_then_shows_the_cursor() {
    assert_eq!(restore_sequence(), b"\x1b[?1049l\x1b[?25h");
}
