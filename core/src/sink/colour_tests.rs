use super::*;

/// The lines `auto-package build` prints while it looks for a source
/// repository, as the sink writes them.
const SAMPLE: &str = "note: home:<user> builds for openSUSE_Tumbleweed and 15.5, Leap 15.3, Leap 15.4; using openSUSE_Tumbleweed (--distro picks another)
info: searching for source repository candidates for: jay
  searching Codeberg for: jay
  searching GitHub API for: jay
  checking openSUSE:Factory for jay
  search request failed: https://codeberg.org/api/v1/repos/search?q=jay...: http status: 429 (rate-limited; set GITLAB_TOKEN ...)
  found 2 matching repos via GitHub API
  found via GitHub search: https://github.com/mahkoh/jay
  checking: https://github.com/python/jay
  not used automatically (not the project's recorded upstream): https://github.com/mahkoh/jay
  ✗ no verified repositories found after trying 7 URLs";

/// `text` without its SGR sequences (`ESC [ ... m`).
fn strip_sgr(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn each_label_is_bold_and_its_own_colour() {
    assert_eq!(colour_diag("note: x", true), "\x1b[1;36mnote:\x1b[22;39m x");
    assert_eq!(colour_diag("info: x", true), "\x1b[1;34minfo:\x1b[22;39m x");
    assert_eq!(
        colour_diag("warning: x", true),
        "\x1b[1;33mwarning:\x1b[22;39m x"
    );
    assert_eq!(
        colour_diag("error: x", true),
        "\x1b[1;31merror:\x1b[22;39m x"
    );
}

#[test]
fn a_label_needs_the_space_after_it() {
    assert_eq!(colour_diag("notes: x", true), "notes: x");
    assert_eq!(colour_diag("  note: x", true), "  note: x");
}

#[test]
fn ticks_and_crosses_colour_the_whole_line() {
    assert_eq!(
        colour_diag("✓ built jay", true),
        "\x1b[32m✓ built jay\x1b[39m"
    );
    assert_eq!(
        colour_diag("  ✗ no verified repositories", true),
        "  \x1b[31m✗ no verified repositories\x1b[39m"
    );
}

#[test]
fn sub_step_verbs_and_the_result_word() {
    assert_eq!(
        colour_diag("  searching Codeberg for: jay", true),
        "  \x1b[2msearching\x1b[22m Codeberg for: jay"
    );
    assert_eq!(
        colour_diag("  checking: https://github.com/python/jay", true),
        "  \x1b[2mchecking\x1b[22m: \x1b[4mhttps://github.com/python/jay\x1b[24m"
    );
    assert_eq!(
        colour_diag("  found 2 matching repos via GitHub API", true),
        "  \x1b[32mfound\x1b[39m 2 matching repos via GitHub API"
    );
    // A word that only starts with a verb is not one.
    assert_eq!(colour_diag("  foundry for jay", true), "  foundry for jay");
}

#[test]
fn urls_are_underlined_without_their_trailing_punctuation() {
    assert_eq!(
        colour_diag(
            "  found via GitHub search: https://github.com/mahkoh/jay",
            true
        ),
        "  \x1b[32mfound\x1b[39m via GitHub search: \x1b[4mhttps://github.com/mahkoh/jay\x1b[24m"
    );
    assert_eq!(
        colour_diag("see (https://a.example/b).", true),
        "see (\x1b[4mhttps://a.example/b\x1b[24m)."
    );
}

#[test]
fn problem_phrases_are_yellow() {
    assert_eq!(
        colour_diag("  not used automatically (not the upstream)", true),
        "  \x1b[33mnot used automatically\x1b[39m (not the upstream)"
    );
    assert_eq!(
        colour_diag("  search request failed: x", true),
        "  search request \x1b[33mfailed\x1b[39m: x"
    );
    assert_eq!(
        colour_diag("(rate-limited; retry)", true),
        "(\x1b[33mrate-limited\x1b[39m; retry)"
    );
    // Inside a red line the yellow span hands back to red.
    assert_eq!(
        colour_diag("✗ failed", true),
        "\x1b[31m✗ \x1b[33mfailed\x1b[31m\x1b[39m"
    );
}

#[test]
fn an_unrecognised_line_is_unchanged() {
    let line = "building jay 1.2.3 in /tmp/x";
    assert_eq!(colour_diag(line, true), line);
}

#[test]
fn stripping_the_escapes_gives_the_input_back() {
    assert_eq!(strip_sgr(&colour_diag(SAMPLE, true)), SAMPLE);
    let odd = "note: 100% {done} {0} {:?}\n  checking: https://x.example/{a}%20b\n\n✗ 50% failed";
    assert_eq!(strip_sgr(&colour_diag(odd, true)), odd);
}

#[test]
fn plain_mode_returns_the_input_byte_for_byte() {
    assert_eq!(colour_diag(SAMPLE, false), SAMPLE);
    let odd = "error: {x} %s\n\n  found \u{e9}t\u{e9} https://a.example";
    assert_eq!(colour_diag(odd, false), odd);
}

#[test]
fn each_line_of_a_multi_line_message_is_coloured_alone() {
    assert_eq!(
        colour_diag("warning: a\n  checking: x", true),
        "\x1b[1;33mwarning:\x1b[22;39m a\n  \x1b[2mchecking\x1b[22m: x"
    );
    // A leading newline keeps its place; the line after it is still coloured.
    assert_eq!(
        colour_diag("\nerror: stopped", true),
        "\n\x1b[1;31merror:\x1b[22;39m stopped"
    );
}

#[test]
fn a_line_with_an_escape_already_is_left_alone() {
    let line = "note: \u{1b}[31mred\u{1b}[0m";
    assert_eq!(colour_diag(line, true), line);
}

#[test]
fn the_sample_lines_get_the_expected_labels_and_marks() {
    let out = colour_diag(SAMPLE, true);
    assert!(out.starts_with("\x1b[1;36mnote:\x1b[22;39m home:<user>"));
    assert!(out.contains("  \x1b[2msearching\x1b[22m Codeberg for: jay"));
    assert!(out.contains("\x1b[4mhttps://codeberg.org/api/v1/repos/search?q=jay\x1b[24m"));
    assert!(out.contains("\x1b[33mrate-limited\x1b[39m"));
    assert!(
        out.ends_with("  \x1b[31m✗ no verified repositories found after trying 7 URLs\x1b[39m")
    );
}
