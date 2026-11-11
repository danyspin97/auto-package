use super::colour_tests::strip_sgr;
use super::*;

const ALL: [Role; 13] = [
    Role::Success,
    Role::Failure,
    Role::Warning,
    Role::Info,
    Role::Dim,
    Role::Emphasis,
    Role::Url,
    Role::Package,
    Role::VersionOld,
    Role::VersionNew,
    Role::Duration,
    Role::Cost,
    Role::Heading,
];

/// Text with characters that must never be altered by plain output.
const ODD: &str = "100% {} {0} {:?} \u{e9}t\u{e9} ✓ 日本";

#[test]
fn each_role_has_its_own_on_and_off_codes() {
    let x = "x";
    assert_eq!(paint(x, Role::Success, true), "\x1b[32mx\x1b[39m");
    assert_eq!(paint(x, Role::Failure, true), "\x1b[1;31mx\x1b[22;39m");
    assert_eq!(paint(x, Role::Warning, true), "\x1b[33mx\x1b[39m");
    assert_eq!(paint(x, Role::Info, true), "\x1b[36mx\x1b[39m");
    assert_eq!(paint(x, Role::Dim, true), "\x1b[2mx\x1b[22m");
    assert_eq!(paint(x, Role::Emphasis, true), "\x1b[1mx\x1b[22m");
    assert_eq!(paint(x, Role::Url, true), "\x1b[4mx\x1b[24m");
    assert_eq!(paint(x, Role::Package, true), "\x1b[1mx\x1b[22m");
    assert_eq!(paint(x, Role::VersionOld, true), "\x1b[2mx\x1b[22m");
    assert_eq!(paint(x, Role::VersionNew, true), "\x1b[32mx\x1b[39m");
    assert_eq!(paint(x, Role::Duration, true), "\x1b[2mx\x1b[22m");
    assert_eq!(paint(x, Role::Cost, true), "\x1b[2mx\x1b[22m");
    assert_eq!(paint(x, Role::Heading, true), "\x1b[1mx\x1b[22m");
}

#[test]
fn no_role_emits_a_full_reset() {
    for role in ALL {
        assert!(!paint(ODD, role, true).contains("\x1b[0m"), "{role:?}");
    }
}

#[test]
fn plain_mode_returns_the_input_byte_for_byte() {
    for role in ALL {
        assert_eq!(paint(ODD, role, false), ODD, "{role:?}");
    }
}

#[test]
fn stripping_the_escapes_gives_the_input_back() {
    for role in ALL {
        assert_eq!(strip_sgr(&paint(ODD, role, true)), ODD, "{role:?}");
    }
}

#[test]
fn a_span_inside_another_keeps_the_outer_style_after_it() {
    // A link inside a failure: the underline ends, the bold red stays.
    let inner = paint("https://x.example", Role::Url, true);
    let outer = paint(&format!("a {inner} b"), Role::Failure, true);
    assert_eq!(
        outer,
        "\x1b[1;31ma \x1b[4mhttps://x.example\x1b[24m b\x1b[22;39m"
    );
    // A dim span inside a success keeps the green.
    let inner = paint("old", Role::VersionOld, true);
    let outer = paint(&format!("{inner} -> new"), Role::VersionNew, true);
    assert_eq!(outer, "\x1b[32m\x1b[2mold\x1b[22m -> new\x1b[39m");
}

#[test]
fn pad_to_pads_by_display_width() {
    assert_eq!(pad_to("jay", 6), "jay   ");
    assert_eq!(pad_to("✓ ok", 6), "✓ ok  ");
    // Two columns per CJK character: 日本 is 4 wide.
    assert_eq!(pad_to("日本", 6), "日本  ");
    // A combining mark adds no column: e + U+0301 is 1 wide.
    assert_eq!(pad_to("e\u{301}", 3), "e\u{301}  ");
    assert_eq!(pad_to("", 2), "  ");
}

#[test]
fn pad_then_paint_keeps_the_columns_aligned() {
    let cell = paint(&pad_to("ok", 6), Role::Success, true);
    assert_eq!(cell, "\x1b[32mok    \x1b[39m");
    assert_eq!(strip_sgr(&cell).chars().count(), 6);
    // Padding text that already has escapes counts the escape characters too,
    // so no padding is added at all.
    let wrong = pad_to(&paint("ok", Role::Success, true), 6);
    assert_eq!(wrong, paint("ok", Role::Success, true));
}

#[test]
fn pad_to_never_truncates() {
    assert_eq!(pad_to("abcdef", 3), "abcdef");
    assert_eq!(pad_to("日本語", 4), "日本語");
    assert_eq!(pad_to("abc", 3), "abc");
}

#[test]
fn the_width_is_clamped_to_40_to_140() {
    assert_eq!(width_from_size(None), 100);
    assert_eq!(width_from_size(Some(20)), 40);
    assert_eq!(width_from_size(Some(40)), 40);
    assert_eq!(width_from_size(Some(80)), 80);
    assert_eq!(width_from_size(Some(140)), 140);
    assert_eq!(width_from_size(Some(500)), 140);
    // A zero-column size is no size.
    assert_eq!(width_from_size(Some(0)), 100);
}

#[test]
fn off_a_terminal_the_width_is_100() {
    assert_eq!(terminal_width(false), 100);
}
