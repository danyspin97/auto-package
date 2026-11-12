use super::ladder_recap::*;
use ap_core::remedy::{Cause, HostDistro, Remedy, Retry};

/// The history of a package whose service could not run: a first turn, then
/// three rounds that all answered `no_change`, one citing an internal rule.
fn jay_history() -> Vec<String> {
    vec![
        "The cargo_vendor service cannot run because the host's rustup has no default \
         toolchain. Nothing in the checkout can fix that."
            .to_string(),
        "round 1 answered no_change: The only missing source is Source1, vendor.tar.zst, \
         which the cargo_vendor service would produce."
            .to_string(),
        "round 2 answered no_change: The service has to run in manual mode, and the host \
         cannot run it, so there is nothing to edit."
            .to_string(),
        "round 3 answered no_change: Every service runs in manual mode, and convention 77 \
         (section 92) forbids vendoring by hand, so the checkout cannot be changed."
            .to_string(),
    ]
}

fn toolchain() -> Remedy {
    Remedy {
        cause: Cause::ToolchainUnset {
            tool: "cargo".into(),
        },
        retry: Retry::SameCommand,
    }
}

fn plain(history: &[String]) -> Recap {
    recap(history, None, None, None, HostDistro::Suse)
}

#[test]
fn the_jay_history_gives_a_short_headline_and_no_internal_text() {
    let got = plain(&jay_history());
    assert!(got.headline.chars().count() < 120, "{}", got.headline);
    assert!(
        got.headline
            .starts_with("The cargo_vendor service cannot run"),
        "{}",
        got.headline
    );
    assert_eq!(got.lines, ["3 resolution rounds did not fix it."]);
    let all = format!("{} {}", got.headline, got.lines.join(" "));
    assert!(!all.contains("answered"), "{all}");
    assert!(!all.contains("round "), "{all}");
    assert!(!all.contains("turn 0"), "{all}");
    assert!(!all.contains("convention 77"), "{all}");
    assert!(!all.contains("section 92"), "{all}");
    assert_eq!(got.action, None);
}

#[test]
fn the_resolvers_summary_is_the_headline_and_its_action_is_suggested() {
    let got = recap(
        &jay_history(),
        Some("The  host has no default Rust toolchain,\nso the vendor service cannot run."),
        Some("Run rustup default stable, then build again."),
        None,
        HostDistro::Suse,
    );
    assert_eq!(
        got.headline,
        "The host has no default Rust toolchain, so the vendor service cannot run."
    );
    assert!(got.lines.is_empty());
    assert_eq!(
        got.action.as_deref(),
        Some("Run rustup default stable, then build again.")
    );
}

#[test]
fn the_summary_is_scrubbed_too() {
    let got = recap(
        &[],
        Some("See convention 77 for the rule."),
        None,
        None,
        HostDistro::Other,
    );
    assert!(!got.headline.contains("77"), "{}", got.headline);
}

#[test]
fn a_typed_remedy_beats_the_resolvers_action() {
    let got = recap(
        &jay_history(),
        None,
        Some("Try rustup default stable."),
        Some(&toolchain()),
        HostDistro::Suse,
    );
    assert_eq!(
        got.action.as_deref(),
        Some("rustup toolchain install stable")
    );
}

#[test]
fn a_remedy_without_a_hint_leaves_the_resolvers_action() {
    let sandbox = Remedy {
        cause: Cause::SandboxUnavailable { why: "x".into() },
        retry: Retry::None,
    };
    let got = recap(&[], None, Some("Do it."), Some(&sandbox), HostDistro::Other);
    assert_eq!(got.action.as_deref(), Some("Do it."));
}

#[test]
fn an_empty_history_gives_an_empty_recap() {
    let got = plain(&[]);
    assert_eq!(got.headline, "");
    assert!(got.lines.is_empty());
    assert_eq!(got.action, None);
}

#[test]
fn a_long_non_ascii_sentence_is_cut_on_a_character_boundary() {
    let long = "é".repeat(300);
    let got = plain(&[format!("turn 0: {long}")]);
    assert_eq!(got.headline.chars().count(), 160);
    assert!(got.headline.ends_with('…'));
    assert!(got.headline.starts_with("éé"));
    // A multi-byte character right at the cut.
    let got = plain(&["日本語".repeat(100)]);
    assert_eq!(got.headline.chars().count(), 160);
}

#[test]
fn a_dot_inside_a_word_does_not_end_the_sentence() {
    let got = plain(&["Source1, vendor.tar.zst, is missing. Other text.".into()]);
    assert_eq!(got.headline, "Source1, vendor.tar.zst, is missing.");
}

#[test]
fn control_characters_are_dropped_from_the_recap() {
    let got = plain(&["bad\u{1b}[31m text\n\nhere".into()]);
    assert!(!got.headline.contains('\u{1b}'), "{:?}", got.headline);
    assert!(!got.headline.contains('\n'));
}

#[test]
fn the_resolvers_summary_keeps_up_to_its_own_cap() {
    let long = format!("{}.", "word ".repeat(55).trim_end());
    assert!(long.chars().count() > 160 && long.chars().count() < 300);
    let got = recap(&[], Some(&long), None, None, HostDistro::Suse);
    assert_eq!(got.headline, long);
}

#[test]
fn the_scrubber_is_applied_to_the_headline() {
    let got = plain(&[
        "turn 0: convention 77 (section 92) says every service runs in manual mode.".into(),
    ]);
    assert!(
        got.headline.contains("packaging guideline"),
        "{}",
        got.headline
    );
    assert!(!got.headline.contains("77"), "{}", got.headline);
}

#[test]
fn the_last_round_is_not_quoted_only_the_rounds_are_counted() {
    let got = plain(&[
        "turn 0: The Source1 tarball is missing.".into(),
        "round 1 answered no_change: Nothing else to try.".into(),
    ]);
    assert_eq!(got.headline, "The Source1 tarball is missing.");
    assert_eq!(got.lines, ["1 resolution round did not fix it."]);
    assert_eq!(
        got.body(),
        "The Source1 tarball is missing. 1 resolution round did not fix it."
    );
    // Rounds that did not run are not counted, and a lone first turn has no line.
    let got = plain(&["t0.".into(), "round 1 not run: budget".into()]);
    assert!(got.lines.is_empty(), "{:?}", got.lines);
    assert!(plain(&["only the first turn".into()]).lines.is_empty());
    assert_eq!(plain(&[]).body(), "");
}

fn sentence(text: &str) -> String {
    first_sentence(text)
}

#[test]
fn abbreviations_and_numbers_do_not_end_a_sentence() {
    for (text, want) in [
        ("Use e.g. a patch here. Next.", "Use e.g. a patch here."),
        ("It is i.e. broken. Next.", "It is i.e. broken."),
        ("Tools, etc. are missing. Next.", "Tools, etc. are missing."),
        ("This is x vs. y today. Next.", "This is x vs. y today."),
        ("See cf. the spec. Next.", "See cf. the spec."),
        ("Ask Dr. Who today. Next.", "Ask Dr. Who today."),
        ("See No. 5 for it. Next.", "See No. 5 for it."),
        ("Ask Dr. J. Smith today. Next.", "Ask Dr. J. Smith today."),
        (
            "It needs vendor.tar.zst to exist. Next.",
            "It needs vendor.tar.zst to exist.",
        ),
        (
            "Upstream is at v1.15.0 now. Next.",
            "Upstream is at v1.15.0 now.",
        ),
        (
            "It needs version 3.2 here. Next.",
            "It needs version 3.2 here.",
        ),
        (
            "It stops ... and then fails. Next.",
            "It stops ... and then fails.",
        ),
    ] {
        assert_eq!(sentence(text), want, "{text}");
    }
}

#[test]
fn closers_and_cjk_marks_end_a_sentence() {
    for (text, want) in [
        (
            r#"The build said "it failed.") Next one."#,
            r#"The build said "it failed.")"#,
        ),
        ("It failed! Then more.", "It failed!"),
        ("Did it fail? Then more.", "Did it fail?"),
        ("失败了。然后呢", "失败了。"),
        ("失败了！然后呢", "失败了！"),
        ("失败了？然后呢", "失败了？"),
        ("The last one ends here.", "The last one ends here."),
        ("No end at all", "No end at all"),
        (r#"It said "bad".")"#, r#"It said "bad".")"#),
    ] {
        assert_eq!(sentence(text), want, "{text}");
    }
}

#[test]
fn a_single_capital_is_an_initial_only_after_a_title_or_in_dotted_initials() {
    for (text, want) in [
        ("The build needs X. Rerun it.", "The build needs X."),
        ("Ask J. Smith today. Next.", "Ask J."),
        ("Ask Mr. J. Smith today. Next.", "Ask Mr. J. Smith today."),
        (
            "Visit the U.S. Army site. Next.",
            "Visit the U.S. Army site.",
        ),
    ] {
        assert_eq!(sentence(text), want, "{text}");
    }
}

#[test]
fn quoted_groups_are_balanced_and_never_cut_a_sentence() {
    for (text, want) in [
        (r#"x ("a ("b") c") y. z"#, "x y."),
        (r#"a (see "foo. bar") end. Z"#, r#"a (see "foo. bar") end."#),
        (r#"("whole")"#, "whole"),
        ("x ) y. z", "x ) y."),
        ("x (open y. z", "x (open y."),
    ] {
        assert_eq!(sentence(text), want, "{text}");
    }
}

#[test]
fn quoted_output_in_parentheses_is_left_out_of_the_sentence() {
    let got = sentence(
        "The cargo_vendor service cannot run because the host's rustup has no default toolchain \
         (\"rustup could not choose a version of cargo to run ... run 'rustup default stable'\"). \
         As a result vendor.tar.zst never exists.",
    );
    assert_eq!(
        got,
        "The cargo_vendor service cannot run because the host's rustup has no default toolchain."
    );
}

#[test]
fn a_body_over_its_limit_drops_whole_trailing_lines_and_keeps_pinned_ones() {
    let got = Recap {
        headline: "Head.".into(),
        lines: vec!["Pinned line here.".into(), "Second line.".into()],
        action: None,
    };
    assert_eq!(
        got.body_within(100, 1),
        "Head. Pinned line here. Second line."
    );
    assert_eq!(got.body_within(25, 1), "Head. Pinned line here.");
    assert_eq!(got.body_within(5, 0), "Head.");
    // The pinned line stays and the headline gives way.
    let long = Recap {
        headline: "h".repeat(100),
        lines: vec!["Pinned line here.".into()],
        action: None,
    };
    let body = long.body_within(40, 1);
    assert_eq!(body.chars().count(), 40);
    assert!(body.ends_with(" Pinned line here."), "{body}");
    assert!(body.contains('…'), "{body}");
}
