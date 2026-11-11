use crate::user_text::scrub_internal_refs;

#[test]
fn replaces_the_reference_and_keeps_the_sentence() {
    let text = "Moving cargo_vendor to server mode: convention 77 (section 92) says every service runs in manual mode";
    let out = scrub_internal_refs(text);
    assert_eq!(
        out,
        "Moving cargo_vendor to server mode: the packaging guideline says every service runs in manual mode"
    );
    assert!(!out.to_ascii_lowercase().contains("convention"));
    assert!(!out.contains("77") && !out.contains("92"));
}

#[test]
fn table_of_references_and_their_replacements() {
    let cases = [
        ("Convention 77 says X", "The packaging guideline says X"),
        ("The convention 5 says X", "The packaging guideline says X"),
        (
            "see convention 5 for details",
            "see the packaging guideline for details",
        ),
        (
            "per convention 5, do X",
            "per the packaging guideline, do X",
        ),
        (
            "conventions 3, 4 and 5 apply",
            "the packaging guidelines apply",
        ),
        (
            "convention 5 (section 4)'s badness",
            "the packaging guideline's badness",
        ),
        (
            "Rule convention 5's applies here.",
            "Rule the packaging guideline's applies here.",
        ),
        ("section 4 of convention 5", "the packaging guideline"),
        ("section 4 of the conventions", "the packaging guideline"),
        (
            "Section 4 of Convention 5 applies.",
            "the packaging guideline applies.",
        ),
        (
            "convention 3, section 4 applies.",
            "the packaging guideline applies.",
        ),
        (
            "convention 3 section 4 applies.",
            "the packaging guideline applies.",
        ),
        (
            "convention 3 and section 4 of the manual",
            "the packaging guideline and section 4 of the manual",
        ),
        (
            "Read convention 3, 2024 applies.",
            "Read the packaging guideline, 2024 applies.",
        ),
        (
            "Read convention 3, 2 files.",
            "Read the packaging guideline, 2 files.",
        ),
        (
            "Read conventions 3, 45, 678 and 9 apply.",
            "Read the packaging guidelines apply.",
        ),
        (
            "Lists conventions 3, 4 & 6-8 apply.",
            "Lists the packaging guidelines apply.",
        ),
        (
            "Lists conventions 3, 4 and 1024 apply.",
            "Lists the packaging guidelines and 1024 apply.",
        ),
        ("convention 3 files", "the packaging guideline files"),
        (
            "Rule (convention 2) and (convention 3).",
            "Rule (the packaging guideline) and (the packaging guideline).",
        ),
        (
            "Ref convention 3 2 4 end",
            "Ref the packaging guideline 2 4 end",
        ),
        (
            "He says convention 3 applies.",
            "He says the packaging guideline applies.",
        ),
        ("docs, convention 3.", "docs, the packaging guideline."),
        ("a, convention 3, b", "a, the packaging guideline, b"),
        (
            "Build per the convention 5.",
            "Build per the packaging guideline.",
        ),
        (
            "Open conventions/77.md now.",
            "Open the packaging guidelines now.",
        ),
        (
            "Open build/conventions/12.md now.",
            "Open the packaging guidelines now.",
        ),
        (
            "Open the conventions/77.md now.",
            "Open the packaging guidelines now.",
        ),
        ("(convention 4) end", "(the packaging guideline) end"),
    ];
    for (input, expected) in cases {
        assert_eq!(scrub_internal_refs(input), expected, "input: {input:?}");
    }
}

#[test]
fn text_that_only_looks_like_a_reference_is_unchanged() {
    let cases = [
        "",
        "plain text, nothing to see",
        "a convention of the group has no number",
        "https://example.org/conventions/12.md",
        "see https://example.org/build/conventions/12.md",
        "my-conventions/12.md",
        "x/build/conventions/12.md",
        "conventions/12.md/extra",
        "conventions/12.mdx",
        "a.convention 5 b",
        "foo -convention 5",
        "unconvention 5",
        "conventional 5",
        "See the docs. Section 2 covers packaging.",
        "section 3 of the manual is the reference.",
        "the [package] section of Cargo.toml",
        "the (section 3) of text",
        "(section 92) on its own",
        "Empty () and [] stay.",
    ];
    for input in cases {
        assert_eq!(scrub_internal_refs(input), input, "input: {input:?}");
    }
}

#[test]
fn non_ascii_text_is_preserved() {
    assert_eq!(
        scrub_internal_refs("✓ convention 3 done — 完成 (convention 4) café"),
        "✓ the packaging guideline done — 完成 (the packaging guideline) café"
    );
    let plain = "✓ done — 完成 café naïve e\u{301} 🎉 end";
    assert_eq!(scrub_internal_refs(plain), plain);
}

#[test]
fn scrubbing_is_idempotent_on_realistic_inputs() {
    let inputs = [
        "Moving cargo_vendor to server mode: convention 77 (section 92) says every service runs in manual mode",
        "convention 3 section 4 section 5",
        "per convention 3 see convention 4.",
        "xx(section 3)convention 4",
        "a, convention 3, , b",
        "Note: convention 3 (convention 4) end.",
        "Section 4 of convention 5, section 6 of the docs.",
        "Read convention 3, 2024 applies.",
        "Read conventions 3, 45, 678 and 9 apply.",
        "See the docs. Section 2 covers packaging.",
        "the the convention 5",
    ];
    for input in inputs {
        let once = scrub_internal_refs(input);
        assert_eq!(scrub_internal_refs(&once), once, "input: {input:?}");
    }
}

#[test]
fn empty_and_long_input_do_not_panic_and_stay_linear() {
    assert_eq!(scrub_internal_refs(""), "");
    let repeats = "convention 1,".repeat(1_000_000);
    assert_eq!(
        scrub_internal_refs(&repeats),
        "the packaging guideline,".repeat(1_000_000)
    );
    let prose = "é✓ 完成 ".repeat(50_000);
    assert_eq!(scrub_internal_refs(&prose), prose);
    let digits = format!("convention {}", "9".repeat(100_000));
    assert_eq!(scrub_internal_refs(&digits), "the packaging guideline");
    let open = "(section ".repeat(20_000);
    assert_eq!(scrub_internal_refs(&open), open);
}

/// Deterministic pseudo-random strings built from fragments that exercise
/// every rule. The output is idempotent and the function never panics.
#[test]
fn random_fragments_are_idempotent() {
    const FRAGMENTS: [&str; 30] = [
        "convention ",
        "conventions ",
        "Convention ",
        "The ",
        "the ",
        "section ",
        "Section ",
        "sections ",
        "(",
        ")",
        "[",
        "]",
        ", ",
        " and ",
        " or ",
        " of ",
        " of the ",
        "3",
        "42",
        "2024",
        " ",
        "'s ",
        "’s ",
        "é",
        "✓",
        ".",
        "\n",
        "conventions/7.md",
        "build/",
        "-",
    ];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    for _ in 0..3000 {
        let mut input = String::new();
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = (seed >> 33) % 30;
        for _ in 0..len {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            input.push_str(FRAGMENTS[((seed >> 33) as usize) % FRAGMENTS.len()]);
        }
        let once = scrub_internal_refs(&input);
        assert_eq!(scrub_internal_refs(&once), once, "input: {input:?}");
    }
}
