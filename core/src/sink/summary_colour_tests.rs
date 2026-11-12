use super::*;
use crate::sink::colour_tests::strip_sgr;

/// A run summary as the builder writes it: headings, rows, a flag line, a
/// URL, a version change and a hosts section.
const SAMPLE: &str = "\nBuilt this run (2):
  jay 1.0 -> 1.1
  lsvpd 2.0 → 2.1 (requires the update of libfoo)
Needs a maintainer (1):
  gamma: signature refused, see https://build.example/key.
Flagged by the fix loop (1):
  delta
    adds a %post scriptlet
Hosts refused this run (1):
  host codeberg.org is not allowed; allow it for this run with --allow-host codeberg.org
  ✓ built jay
  ✗ failed beta
  ⇥ skipped zeta
  ... and 3 more";

#[test]
fn plain_mode_returns_the_input_byte_for_byte() {
    assert_eq!(colour_summary(SAMPLE, false), SAMPLE);
    let odd = "  a\tb  \n\n  \u{1b}[31mred\n x -> \u{1b}[0m";
    assert_eq!(colour_summary(odd, false), odd);
}

#[test]
fn stripping_the_escapes_gives_the_input_back() {
    assert_eq!(strip_sgr(&colour_summary(SAMPLE, true)), SAMPLE);
    let odd = "  jay  1.0 ->  1.1 \n\u{2068}wide 字 combining e\u{301}\n  x -> ";
    assert_eq!(strip_sgr(&colour_summary(odd, true)), odd);
}

#[test]
fn no_escape_comes_back_when_colour_is_off() {
    assert!(!colour_summary(SAMPLE, false).contains('\u{1b}'));
}

#[test]
fn a_heading_is_bold_and_its_count_is_plain() {
    assert_eq!(
        colour_summary("Built this run (12):", true),
        "\x1b[1mBuilt this run\x1b[22m (12):"
    );
    assert_eq!(
        colour_summary("not started (the run was interrupted) (3):", true),
        "\x1b[1mnot started (the run was interrupted)\x1b[22m (3):"
    );
}

#[test]
fn a_line_that_is_not_a_heading_is_not_bold() {
    assert_eq!(colour_summary("update summary", true), "update summary");
    assert_eq!(colour_summary("Built (x):", true), "Built (x):");
    assert_eq!(
        colour_summary("2 submit request(s) failed", true),
        "2 submit request(s) failed"
    );
}

#[test]
fn a_version_change_is_old_dim_then_new_green() {
    assert_eq!(
        colour_summary("  jay 1.0 -> 1.1", true),
        "  \x1b[1mjay\x1b[22m \x1b[2m1.0\x1b[22m -> \x1b[32m1.1\x1b[39m"
    );
    assert_eq!(
        colour_summary("    1.0 → 1.1", true),
        "    \x1b[2m1.0\x1b[22m → \x1b[32m1.1\x1b[39m"
    );
}

#[test]
fn a_padded_version_change_is_coloured_too() {
    // The old-version cell is padded with spaces before the arrow.
    assert_eq!(
        colour_summary("  jay   1.0     -> 1.1", true),
        "  \x1b[1mjay\x1b[22m   \x1b[2m1.0\x1b[22m     -> \x1b[32m1.1\x1b[39m"
    );
    assert_eq!(
        colour_summary("  jay   1.0     → 1.1 (requires x)", true),
        "  \x1b[1mjay\x1b[22m   \x1b[2m1.0\x1b[22m     → \x1b[32m1.1\x1b[39m (requires x)"
    );
}

#[test]
fn padded_rows_strip_back_to_their_input() {
    let rows = "  jay   1.0     → 1.1\n  lsvpd 10.2.3 → 11\n  ab    1 -> 2  (x)";
    assert_eq!(strip_sgr(&colour_summary(rows, true)), rows);
    assert_eq!(colour_summary(rows, false), rows);
}

#[test]
fn an_unpadded_change_is_the_same_as_before() {
    assert_eq!(
        colour_summary("  jay 1.0 -> 1.1", true),
        "  \x1b[1mjay\x1b[22m \x1b[2m1.0\x1b[22m -> \x1b[32m1.1\x1b[39m"
    );
}

#[test]
fn a_package_name_is_only_painted_on_a_two_space_row() {
    assert_eq!(colour_summary("  jay", true), "  \x1b[1mjay\x1b[22m");
    assert_eq!(colour_summary("  ... and 3 more", true), "  ... and 3 more");
    assert_eq!(colour_summary("    jay", true), "    jay");
}

#[test]
fn a_hosts_row_names_no_package() {
    let row = "  host codeberg.org is not allowed";
    assert_eq!(
        colour_summary(&format!("Hosts refused this run (1):\n{row}"), true),
        "\x1b[1mHosts refused this run\x1b[22m (1):\n  host codeberg.org is not allowed"
    );
}

#[test]
fn a_url_is_underlined_without_its_trailing_punctuation() {
    assert_eq!(
        colour_summary("    see (https://a.example/b).", true),
        "    see (\x1b[4mhttps://a.example/b\x1b[24m)."
    );
}

#[test]
fn ticks_crosses_and_skips_colour_their_whole_row() {
    assert_eq!(
        colour_summary("  ✓ built jay", true),
        "  \x1b[32m✓ built jay\x1b[39m"
    );
    assert_eq!(
        colour_summary("  ✗ failed beta", true),
        "  \x1b[1;31m✗ failed beta\x1b[22;39m"
    );
    assert_eq!(
        colour_summary("  ⇥ skipped zeta", true),
        "  \x1b[2m⇥ skipped zeta\x1b[22m"
    );
}

#[test]
fn a_line_with_an_escape_already_is_left_alone() {
    let line = "  \x1b[31mjay\x1b[39m 1.0 -> 1.1";
    assert_eq!(colour_summary(line, true), line);
}

#[test]
fn each_line_is_coloured_alone_and_newlines_are_kept() {
    assert_eq!(
        colour_summary("\nBuilt (1):\n  jay", true),
        "\n\x1b[1mBuilt\x1b[22m (1):\n  \x1b[1mjay\x1b[22m"
    );
}

#[test]
fn the_sample_has_the_expected_golden_lines() {
    let out = colour_summary(SAMPLE, true);
    let lines: Vec<&str> = out.split('\n').collect();
    assert_eq!(lines[1], "\x1b[1mBuilt this run\x1b[22m (2):");
    assert_eq!(
        lines[2],
        "  \x1b[1mjay\x1b[22m \x1b[2m1.0\x1b[22m -> \x1b[32m1.1\x1b[39m"
    );
    assert_eq!(
        lines[3],
        "  \x1b[1mlsvpd\x1b[22m \x1b[2m2.0\x1b[22m → \x1b[32m2.1\x1b[39m (requires the update of libfoo)"
    );
    assert_eq!(
        lines[5],
        "  \x1b[1mgamma\x1b[22m: signature refused, see \x1b[4mhttps://build.example/key\x1b[24m."
    );
    assert_eq!(
        lines[10],
        "  host codeberg.org is not allowed; allow it for this run with --allow-host codeberg.org"
    );
}

#[test]
fn a_word_with_a_wide_character_keeps_its_spacing() {
    let line = "  字 1.0 -> 1.1";
    assert_eq!(strip_sgr(&colour_summary(line, true)), line);
}

/// The tries block of the run summary with the user's sample row.
const TRIES_SAMPLE: &str = "Tries to build (1):
  3+ tries: 1
    jay-compositor  4 tries, built · last failed try: install failed";

/// The needs-review section with a grouped service line and a host line.
const REVIEW_SAMPLE: &str = "Needs review (1):
  jay-compositor
    risk  new source services: tar_scm, recompress, set_version, cargo_vendor
    risk  new source host: github.com";

#[test]
fn a_tries_row_is_bold_name_green_built_and_dim_reason() {
    assert_eq!(
        colour_summary(TRIES_SAMPLE, true),
        "\x1b[1mTries to build\x1b[22m (1):\n  3+ tries: 1\n    \x1b[1mjay-compositor\x1b[22m  4 tries, \x1b[32mbuilt\x1b[39m · last failed try: \x1b[2minstall failed\x1b[22m"
    );
}

#[test]
fn a_not_built_row_is_red_and_its_shape_plain() {
    assert_eq!(
        colour_summary(
            "Tries to build (1):\n    foo  2 tries, not built [build failed] · x",
            true
        ),
        "\x1b[1mTries to build\x1b[22m (1):\n    \x1b[1mfoo\x1b[22m  2 tries, \x1b[1;31mnot built\x1b[22;39m [build failed] · \x1b[2mx\x1b[22m"
    );
    assert_eq!(
        colour_summary("Tries to build (1):\n    foo  1 try, no result · why", true),
        "\x1b[1mTries to build\x1b[22m (1):\n    \x1b[1mfoo\x1b[22m  1 try, \x1b[1;31mno result\x1b[22;39m · \x1b[2mwhy\x1b[22m"
    );
}

#[test]
fn a_wide_name_in_a_tries_row_is_bold_and_the_padding_is_kept() {
    assert_eq!(
        colour_summary("Tries to build (1):\n    日本語  3 tries, built", true),
        "\x1b[1mTries to build\x1b[22m (1):\n    \x1b[1m日本語\x1b[22m  3 tries, \x1b[32mbuilt\x1b[39m"
    );
}

#[test]
fn the_bucket_line_and_the_more_line_of_the_tries_block_are_plain() {
    assert_eq!(
        colour_summary(
            "Tries to build (3):\n  1 try: 1, 3+ tries: 2\n    ... and 6 more",
            true
        ),
        "\x1b[1mTries to build\x1b[22m (3):\n  1 try: 1, 3+ tries: 2\n    ... and 6 more"
    );
}

#[test]
fn a_wrapped_reason_line_of_a_tries_row_is_not_painted_as_a_row() {
    let text = "Tries to build (1):\n    pkg  1 try, not built · a b\n         c d";
    assert_eq!(
        colour_summary(text, true),
        "\x1b[1mTries to build\x1b[22m (1):\n    \x1b[1mpkg\x1b[22m  1 try, \x1b[1;31mnot built\x1b[22;39m · \x1b[2ma b\x1b[22m\n         c d"
    );
}

#[test]
fn a_review_row_has_a_yellow_risk_label_and_plain_text() {
    assert_eq!(
        colour_summary(REVIEW_SAMPLE, true),
        "\x1b[1mNeeds review\x1b[22m (1):\n  \x1b[1mjay-compositor\x1b[22m\n    \x1b[33mrisk\x1b[39m  new source services: tar_scm, recompress, set_version, cargo_vendor\n    \x1b[33mrisk\x1b[39m  new source host: github.com"
    );
}

#[test]
fn a_risk_line_outside_the_review_section_is_not_painted() {
    assert_eq!(colour_summary("    risk  x", true), "    risk  x");
}

#[test]
fn the_new_rows_are_plain_without_colour_and_strip_back_to_their_input() {
    let wide = "Tries to build (1):\n    日本語  3 tries, built · last failed try: a b";
    for text in [TRIES_SAMPLE, REVIEW_SAMPLE, wide] {
        assert!(!colour_summary(text, false).contains('\u{1b}'));
        assert_eq!(colour_summary(text, false), text);
        assert_eq!(strip_sgr(&colour_summary(text, true)), text);
    }
}
