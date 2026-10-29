
use super::*;

#[test]
fn short_text_is_returned_unchanged() {
    assert_eq!(truncate_with_ellipsis("cargo build", 80), "cargo build");
}

#[test]
fn wide_characters_count_as_two_columns() {
    // Nine columns: six for the content, three for "...".
    let s = "日本語テキスト";
    let out = truncate_with_ellipsis(s, 9);
    assert_eq!(out, "日本語...");
    assert!(out.width() <= 9);
}

#[test]
fn a_wide_character_that_does_not_fit_is_dropped_whole() {
    assert_eq!(prefix_within_width("ab日本", 3), "ab");
}

#[test]
fn zero_width_joiner_sequences_are_not_split() {
    // Woman + ZWJ + man + ZWJ + girl: one cluster, six columns wide.
    let family = "👩\u{200d}👨\u{200d}👧";
    assert_eq!(prefix_within_width(family, 6), family);
    assert_eq!(prefix_within_width(family, 5), "");
}

#[test]
fn combining_marks_stay_with_their_base() {
    let s = "e\u{301}x";
    assert_eq!(prefix_within_width(s, 1), "e\u{301}");
}
