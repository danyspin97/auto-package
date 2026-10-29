use super::rollback_note;

#[test]
fn a_rejected_edit_is_written_down_for_the_next_turn() {
    let note = rollback_note("  error: Unterminated {: {python_expand\n ");
    assert!(note.contains("Edit rejected"));
    assert!(note.contains("Unterminated {"));
    assert!(note.contains("redo every change"));
    assert!(rollback_note(&"x".repeat(5000)).len() < 900);
}
