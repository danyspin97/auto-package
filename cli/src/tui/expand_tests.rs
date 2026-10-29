use super::*;

#[test]
fn wrap_breaks_between_words() {
    assert_eq!(wrap_text("one two three", 7), ["one two", "three"]);
    assert_eq!(wrap_text("one two three", 5), ["one", "two", "three"]);
}

#[test]
fn wrap_breaks_a_word_longer_than_the_line() {
    assert_eq!(wrap_text("abcdefgh", 3), ["abc", "def", "gh"]);
}

#[test]
fn wrap_keeps_newlines_and_blank_lines() {
    assert_eq!(wrap_text("a\n\nb", 10), ["a", "", "b"]);
}

#[test]
fn wrap_of_empty_text_has_no_lines() {
    assert!(wrap_text("", 10).is_empty());
}

#[test]
fn wrap_counts_wide_characters_as_two_columns() {
    assert_eq!(wrap_text("日本語", 4), ["日本", "語"]);
}

#[test]
fn wrap_gives_a_glyph_wider_than_the_line_its_own_line() {
    assert_eq!(wrap_text("日", 1), ["日"]);
}

#[test]
fn text_scroll_stays_inside_the_text() {
    assert_eq!(text_scroll_key(KeyCode::Up, 0, 10, 4), 0);
    assert_eq!(text_scroll_key(KeyCode::Down, 0, 10, 4), 1);
    assert_eq!(text_scroll_key(KeyCode::Down, 6, 10, 4), 6);
    assert_eq!(text_scroll_key(KeyCode::PageDown, 0, 10, 4), 3);
    assert_eq!(text_scroll_key(KeyCode::End, 0, 10, 4), 6);
    assert_eq!(text_scroll_key(KeyCode::Home, 6, 10, 4), 0);
}

#[test]
fn esc_closes_an_open_entry_before_quitting() {
    let mut browse = BrowseState::default();
    assert_eq!(esc_action(&browse), EscAction::Quit);
    browse.act_expanded = true;
    assert_eq!(esc_action(&browse), EscAction::Collapse);
}
