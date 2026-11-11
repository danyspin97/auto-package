//! The resolution round's context asks for the user summary and action.

use super::*;

#[test]
fn a_resolver_round_asks_for_the_user_summary_and_action() {
    for round in [1, 2] {
        let text = ladder_context(round, 3, &["Source10 missing".into()], None);
        for needle in [
            "\"user_summary\"",
            "\"user_action\"",
            "Do not mention convention numbers",
            "do not paste logs",
        ] {
            assert!(text.contains(needle), "round {round}: {needle}: {text}");
        }
    }
}
