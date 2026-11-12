use super::*;
use unicode_width::UnicodeWidthStr;

/// `text` without its SGR escape sequences.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for c in text.chars() {
        match c {
            '\u{1b}' => in_code = true,
            'm' if in_code => in_code = false,
            _ if !in_code => out.push(c),
            _ => {}
        }
    }
    out
}

#[test]
fn the_live_line_names_the_state_and_the_elapsed_time() {
    let line = live_line("OBS build: building", Duration::from_secs(129), 100, false);
    assert_eq!(line, "  ▸ OBS build: building… 2m09s");
}

#[test]
fn a_short_wait_shows_its_seconds_with_one_decimal() {
    let line = live_line(
        "OBS build: scheduled",
        Duration::from_millis(12_340),
        100,
        false,
    );
    assert_eq!(line, "  ▸ OBS build: scheduled… 12.3s");
}

#[test]
fn with_colour_off_there_are_no_escape_sequences() {
    let line = live_line("OBS build: building", Duration::from_secs(60), 100, false);
    assert!(!line.contains('\u{1b}'), "{line:?}");
}

#[test]
fn with_colour_on_the_whole_line_is_dim_and_the_text_is_unchanged() {
    let line = live_line("OBS build: building", Duration::from_secs(60), 100, true);
    assert!(line.starts_with("\u{1b}[2m"), "{line:?}");
    assert!(line.ends_with("\u{1b}[22m"), "{line:?}");
    assert_eq!(plain(&line), "  ▸ OBS build: building… 1m00s");
}

#[test]
fn a_long_label_is_cut_so_the_line_fits_one_column_short_of_the_width() {
    let label = "OBS build: blocked: waiting for lib-foo-devel, libbar1, libbaz2 to build";
    for width in [40, 60, 80] {
        let line = live_line(label, Duration::from_secs(129), width, false);
        assert!(
            UnicodeWidthStr::width(line.as_str()) < width,
            "{width}: {line:?}"
        );
        assert!(line.contains('…'), "{line:?}");
        assert!(line.ends_with(" 2m09s"), "{line:?}");
    }
}

#[test]
fn control_characters_in_the_label_become_question_marks() {
    let line = live_line(
        "OBS build:\u{1b}[2K bu\nild",
        Duration::from_secs(9),
        100,
        false,
    );
    assert_eq!(line, "  ▸ OBS build:?[2K bu?ild… 9.0s");
    assert!(!line.contains(['\u{1b}', '\n']), "{line:?}");
}

#[test]
fn a_cjk_label_is_measured_in_display_columns() {
    let line = live_line("OBS build: 构建中", Duration::from_secs(9), 100, false);
    assert_eq!(line, "  ▸ OBS build: 构建中… 9.0s");
    let long = "OBS build: 构建中构建中构建中构建中";
    let cut = live_line(long, Duration::from_secs(129), 20, false);
    assert!(UnicodeWidthStr::width(cut.as_str()) <= 19, "{cut:?}");
    assert!(cut.ends_with("… 2m09s"), "{cut:?}");
}

#[test]
fn a_width_of_twenty_keeps_the_line_within_nineteen_columns() {
    let line = live_line("OBS build: building", Duration::from_secs(129), 20, false);
    assert_eq!(line, "  ▸ OBS buil… 2m09s");
    assert_eq!(UnicodeWidthStr::width(line.as_str()), 19);
}

#[test]
fn the_width_is_the_real_one_and_never_below_ten_columns() {
    assert_eq!(live_width(None), 80);
    assert_eq!(live_width(Some(0)), 80);
    assert_eq!(live_width(Some(30)), 30);
    assert_eq!(live_width(Some(200)), 200);
    assert_eq!(live_width(Some(4)), 10);
}

#[test]
fn a_label_that_fits_is_not_cut() {
    let line = live_line("OBS build: building", Duration::from_secs(9), 40, false);
    assert_eq!(line, "  ▸ OBS build: building… 9.0s");
}
