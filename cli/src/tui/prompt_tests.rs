use super::*;

fn type_str(buf: &mut String, s: &str) {
    for c in s.chars() {
        assert!(matches!(
            prompt_key(buf, KeyCode::Char(c)),
            PromptAction::Pending
        ));
    }
}

#[test]
fn typing_q_or_j_edits_the_buffer_not_the_ui() {
    let mut b = String::new();
    type_str(&mut b, "jq-cpp");
    assert_eq!(b, "jq-cpp");
}

#[test]
fn enter_submits_the_trimmed_name() {
    let mut b = String::new();
    type_str(&mut b, "  oatpp ");
    assert!(matches!(
        prompt_key(&mut b, KeyCode::Enter),
        PromptAction::Submit(n) if n == "oatpp"
    ));
}

#[test]
fn enter_on_blank_and_esc_cancel_and_backspace_edits() {
    let mut b = String::from("  ");
    assert!(matches!(
        prompt_key(&mut b, KeyCode::Enter),
        PromptAction::Cancel
    ));
    let mut b = String::from("ab");
    assert!(matches!(
        prompt_key(&mut b, KeyCode::Backspace),
        PromptAction::Pending
    ));
    assert_eq!(b, "a");
    assert!(matches!(
        prompt_key(&mut b, KeyCode::Esc),
        PromptAction::Cancel
    ));
}
