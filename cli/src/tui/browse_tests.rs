
use super::*;

fn line(message: &str) -> LogLine {
    LogLine {
        level: LogLevel::Info,
        message: message.to_string(),
        emphasis: false,
    }
}

#[test]
fn window_of_an_empty_log_is_empty() {
    assert_eq!(window(0, 5, 0), 0..0);
    assert_eq!(window(0, 5, 3), 0..0);
}

#[test]
fn window_shorter_than_the_viewport_shows_everything() {
    assert_eq!(window(3, 5, 0), 0..3);
    assert_eq!(window(3, 5, 2), 0..3);
}

#[test]
fn window_equal_to_the_viewport_shows_everything() {
    assert_eq!(window(5, 5, 0), 0..5);
    assert_eq!(window(5, 5, 1), 0..5);
}

#[test]
fn window_offset_past_the_top_is_clamped() {
    assert_eq!(window(10, 4, 100), 0..4);
    assert_eq!(window(10, 4, 6), 0..4);
    assert_eq!(window(10, 4, 0), 6..10);
    assert_eq!(window(10, 4, 2), 4..8);
}

#[test]
fn window_with_zero_viewport_is_empty_and_never_underflows() {
    assert_eq!(window(5, 0, 2), 3..3);
}

#[test]
fn scroll_up_at_the_top_stays_and_down_at_the_tail_stays() {
    assert_eq!(scroll_key(KeyCode::Up, 6, 10, 4), 6);
    assert_eq!(scroll_key(KeyCode::Down, 0, 10, 4), 0);
    assert_eq!(scroll_key(KeyCode::Char('k'), 0, 10, 4), 1);
    assert_eq!(scroll_key(KeyCode::Char('j'), 3, 10, 4), 2);
}

#[test]
fn paging_moves_by_viewport_minus_one_with_a_floor_of_one() {
    assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 5), 4);
    assert_eq!(scroll_key(KeyCode::PageDown, 4, 10, 5), 0);
    assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 1), 1);
    assert_eq!(scroll_key(KeyCode::PageUp, 0, 10, 0), 1);
    assert_eq!(scroll_key(KeyCode::PageDown, 0, 10, 0), 0);
}

#[test]
fn home_is_the_oldest_line_and_end_is_the_tail() {
    assert_eq!(scroll_key(KeyCode::Home, 0, 10, 4), 6);
    assert_eq!(scroll_key(KeyCode::End, 6, 10, 4), 0);
    assert_eq!(scroll_key(KeyCode::Home, 0, 0, 4), 0);
}

#[test]
fn scroll_offset_is_clamped_when_content_shrinks() {
    assert_eq!(scroll_key(KeyCode::PageUp, 50, 10, 4), 6);
    assert_eq!(scroll_key(KeyCode::Up, 50, 10, 4), 6);
}

#[test]
fn a_view_at_the_tail_keeps_following() {
    assert_eq!(follow_adjust(0, 10, 4), 0);
}

#[test]
fn a_scrolled_view_moves_up_by_the_lines_that_arrived() {
    assert_eq!(follow_adjust(5, 3, 0), 8);
    assert_eq!(follow_adjust(5, 7, 7), 5);
}

#[test]
fn eviction_after_arrivals_is_left_to_the_clamp() {
    let adjusted = follow_adjust(8, 3, 0);
    assert_eq!(adjusted, 11);
    assert_eq!(clamp_offset(adjusted, 10, 4), 6);
}

#[test]
fn filter_is_case_insensitive_substring() {
    assert!(filter_matches("Building jq-cpp", "JQ"));
    assert!(filter_matches("über cool", "ÜBER"));
    assert!(!filter_matches("nothing here", "xyz"));
}

#[test]
fn empty_filter_matches_every_line() {
    assert!(filter_matches("", ""));
    assert!(filter_matches("anything", ""));
}

#[test]
fn filtered_indices_keep_log_order() {
    let lines = [line("a"), line("B a"), line("c")];
    assert_eq!(filtered_indices(&lines, "a"), vec![0, 1]);
    assert_eq!(filtered_indices(&lines, ""), vec![0, 1, 2]);
    assert!(filtered_indices(&lines, "zzz").is_empty());
}

#[test]
fn focus_cycles_both_ways_and_wraps() {
    assert_eq!(cycle_focus(Focus::Table, false), Focus::Log);
    assert_eq!(cycle_focus(Focus::Log, false), Focus::Activity);
    assert_eq!(cycle_focus(Focus::Activity, false), Focus::Table);
    assert_eq!(cycle_focus(Focus::Table, true), Focus::Activity);
    assert_eq!(cycle_focus(Focus::Activity, true), Focus::Log);
    assert_eq!(cycle_focus(Focus::Log, true), Focus::Table);
}

#[test]
fn activity_selection_stays_inside_an_empty_or_short_list() {
    assert_eq!(activity_move(0, 0, KeyCode::Down, 5), 0);
    assert_eq!(activity_move(4, 2, KeyCode::Down, 5), 1);
    assert_eq!(activity_move(9, 3, KeyCode::Up, 5), 1);
}

#[test]
fn activity_selection_moves_and_jumps() {
    assert_eq!(activity_move(1, 10, KeyCode::Down, 5), 2);
    assert_eq!(activity_move(0, 10, KeyCode::Up, 5), 0);
    assert_eq!(activity_move(0, 10, KeyCode::PageDown, 3), 2);
    assert_eq!(activity_move(5, 10, KeyCode::Home, 3), 0);
    assert_eq!(activity_move(5, 10, KeyCode::End, 3), 9);
    assert_eq!(activity_move(9, 10, KeyCode::Down, 3), 9);
}

#[test]
fn activity_top_is_zero_for_an_empty_list() {
    assert_eq!(activity_top(0, 3, 0, 5), 0);
}

#[test]
fn activity_top_scrolls_the_minimum_to_show_the_selection() {
    assert_eq!(activity_top(7, 0, 17, 5), 3);
    assert_eq!(activity_top(1, 4, 17, 5), 1);
    assert_eq!(activity_top(5, 3, 17, 5), 3);
    assert_eq!(activity_top(16, 0, 17, 5), 12);
}

#[test]
fn activity_selection_stays_visible_after_the_viewport_shrinks() {
    let top = activity_top(7, 5, 17, 2);
    assert!((top..top + 2).contains(&7));
}
