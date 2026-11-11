//! The optional `user_summary` and `user_action` of a reply: parsed, normalised,
//! cut to their caps, and refused when they look like a log paste.

use super::*;
use crate::agent::user_text::*;

/// A reply with the required fields, plus `fields` merged in.
fn reply(fields: serde_json::Value) -> String {
    let mut reply = serde_json::json!({
        "action": "no_change",
        "reasoning": "r",
        "evidence": [],
        "confidence": "high"
    });
    reply
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    reply.to_string()
}

/// The `(user_summary, user_action)` a reply with `fields` parses to.
fn parsed(fields: serde_json::Value) -> (Option<String>, Option<String>) {
    let resp = pick_turn_response(&reply(fields), &[]).expect("the reply parses");
    (resp.user_summary, resp.user_action)
}

#[test]
fn an_old_reply_without_the_fields_still_parses() {
    assert_eq!(parsed(serde_json::json!({})), (None, None));
}

#[test]
fn a_reply_with_both_fields_parses_them() {
    let (summary, action) = parsed(serde_json::json!({
        "user_summary": "The build needs zlib. Nothing else is missing.",
        "user_action": "Add BuildRequires: zlib-devel to the spec."
    }));
    assert_eq!(
        summary.as_deref(),
        Some("The build needs zlib. Nothing else is missing.")
    );
    assert_eq!(
        action.as_deref(),
        Some("Add BuildRequires: zlib-devel to the spec.")
    );
}

#[test]
fn a_reply_with_one_field_parses_that_one() {
    let (summary, action) = parsed(serde_json::json!({"user_summary": "The tests need a server."}));
    assert_eq!(summary.as_deref(), Some("The tests need a server."));
    assert_eq!(action, None);

    let (summary, action) = parsed(serde_json::json!({"user_action": "Run osc build again."}));
    assert_eq!(summary, None);
    assert_eq!(action.as_deref(), Some("Run osc build again."));
}

#[test]
fn an_empty_or_null_field_is_none() {
    for empty in ["", "   ", "\n \n\t"] {
        assert_eq!(
            parsed(serde_json::json!({"user_summary": empty, "user_action": empty})),
            (None, None),
            "{empty:?}"
        );
    }
    assert_eq!(
        parsed(serde_json::json!({"user_summary": null, "user_action": null})),
        (None, None)
    );
}

#[test]
fn whitespace_and_line_breaks_collapse_to_single_spaces() {
    let (summary, _) = parsed(serde_json::json!({
        "user_summary": "  The build\n needs   zlib.\r\nNothing else. "
    }));
    assert_eq!(
        summary.as_deref(),
        Some("The build needs zlib. Nothing else.")
    );
}

#[test]
fn a_summary_of_at_most_three_lines_is_kept_and_a_longer_one_is_a_paste() {
    let (summary, _) = parsed(serde_json::json!({"user_summary": "one\ntwo\nthree"}));
    assert_eq!(summary.as_deref(), Some("one two three"));

    let (summary, action) = parsed(serde_json::json!({
        "user_summary": "error: a\nerror: b\nerror: c\nerror: d",
        "user_action": "Run the tests again."
    }));
    assert_eq!(summary, None, "a log paste is dropped");
    assert_eq!(
        action.as_deref(),
        Some("Run the tests again."),
        "the other field is kept"
    );
}

#[test]
fn a_value_with_a_control_character_is_a_paste_and_dropped() {
    for bad in ["bad\u{0}byte", "bell\u{7}here", "tab\there"] {
        let (summary, action) = parsed(serde_json::json!({
            "user_summary": bad,
            "user_action": bad
        }));
        assert_eq!((summary, action), (None, None), "{bad:?}");
    }
}

#[test]
fn a_summary_of_300_characters_is_kept_whole() {
    let text = "a".repeat(SUMMARY_CAP);
    assert_eq!(
        normalise_user_text(&text, SUMMARY_CAP).as_deref(),
        Some(text.as_str())
    );
}

#[test]
fn a_600_character_summary_is_cut_to_300_with_an_ellipsis() {
    let (summary, _) = parsed(serde_json::json!({"user_summary": "word ".repeat(120)}));
    let summary = summary.expect("a long summary is kept, cut");
    assert_eq!(summary.chars().count(), SUMMARY_CAP);
    assert!(summary.ends_with('…'), "{summary}");
    assert!(!summary.contains("  "), "no double space: {summary}");
    assert!(
        "word "
            .repeat(120)
            .starts_with(summary.trim_end_matches('…').trim_end()),
        "the cut is a prefix of the text"
    );
}

#[test]
fn an_action_is_cut_to_200_characters() {
    let (_, action) = parsed(serde_json::json!({"user_action": "x".repeat(600)}));
    let action = action.expect("a long action is kept, cut");
    assert_eq!(action.chars().count(), ACTION_CAP);
    assert!(action.ends_with('…'));
}

#[test]
fn a_cut_lands_on_a_char_boundary_for_non_ascii_text() {
    // Each unit is two characters of three and four bytes: a byte cut would split one.
    let text = "ß中😀".repeat(200);
    let (summary, _) = parsed(serde_json::json!({"user_summary": text}));
    let summary = summary.expect("kept, cut");
    assert_eq!(summary.chars().count(), SUMMARY_CAP);
    let body = summary.strip_suffix('…').expect("ends with an ellipsis");
    assert!("ß中😀".repeat(200).starts_with(body));
}

#[test]
fn an_extra_field_is_still_tolerated() {
    let (summary, _) = parsed(serde_json::json!({
        "user_summary": "The build needs zlib.",
        "something_new": 3
    }));
    assert_eq!(summary.as_deref(), Some("The build needs zlib."));
}

#[test]
fn a_field_that_is_not_a_string_is_none_and_the_reply_still_parses() {
    for value in [
        serde_json::json!(7),
        serde_json::json!(true),
        serde_json::json!(["a"]),
        serde_json::json!({"a": "b"}),
    ] {
        let text = reply(serde_json::json!({
            "user_summary": value.clone(),
            "user_action": value.clone()
        }));
        let resp = pick_turn_response(&text, &[]).expect("the reply still parses");
        assert_eq!(resp.action, "no_change", "{value}");
        assert_eq!(resp.reasoning, "r", "{value}");
        assert_eq!(
            (resp.user_summary, resp.user_action),
            (None, None),
            "{value}"
        );
    }
    let (summary, action) = parsed(serde_json::json!({
        "user_summary": 7,
        "user_action": "Run osc build again."
    }));
    assert_eq!(summary, None);
    assert_eq!(action.as_deref(), Some("Run osc build again."));
}

#[test]
fn a_spoofing_character_drops_the_value() {
    let spoofing: Vec<char> = ('\u{202A}'..='\u{202E}')
        .chain('\u{2066}'..='\u{2069}')
        .chain(['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}'])
        .collect();
    for c in spoofing {
        let text = format!("The build{c}needs zlib.");
        for cap in [SUMMARY_CAP, ACTION_CAP] {
            assert_eq!(normalise_user_text(&text, cap), None, "U+{:04X}", c as u32);
        }
    }
}

#[test]
fn a_line_separator_is_a_line_break_for_the_count() {
    assert_eq!(
        normalise_user_text("one\u{2028}two\u{2029}three", SUMMARY_CAP).as_deref(),
        Some("one two three")
    );
    assert_eq!(
        normalise_user_text("a\u{2028}b\u{2028}c\u{2028}d", SUMMARY_CAP),
        None
    );
}

#[test]
fn a_lone_carriage_return_is_a_line_break_and_a_crlf_is_one() {
    assert_eq!(
        normalise_user_text("one\rtwo", SUMMARY_CAP).as_deref(),
        Some("one two")
    );
    assert_eq!(normalise_user_text("a\rb\rc\rd", SUMMARY_CAP), None);
    assert_eq!(
        normalise_user_text("a\r\nb\r\nc", SUMMARY_CAP).as_deref(),
        Some("a b c")
    );
}

#[test]
fn a_cut_does_not_leave_a_space_before_the_ellipsis() {
    // The first cap-1 characters end in a space: it is dropped before the ellipsis.
    let text = format!("{} {}", "a".repeat(SUMMARY_CAP - 2), "b".repeat(50));
    let cut = normalise_user_text(&text, SUMMARY_CAP).expect("kept, cut");
    assert_eq!(cut, format!("{}…", "a".repeat(SUMMARY_CAP - 2)));
    assert_eq!(cut.chars().count(), SUMMARY_CAP - 1);
}

#[test]
fn the_schema_lists_both_fields_and_requires_neither() {
    let schema: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::FixAfterBuild)).unwrap();
    for field in ["user_summary", "user_action"] {
        assert_eq!(schema["properties"][field]["type"], "string", "{field}");
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|r| r == field), "{field} is required");
    }
}
