use crate::agent::static_system_prompt;

/// Each fact gets one comment: the rule reaches every turn.
#[test]
fn the_prompts_ask_for_one_comment_per_fact() {
    let p = static_system_prompt();
    assert!(p.contains("**Comment only what would surprise a maintainer.**"));
    assert!(p.contains("does not restate a\n   `BuildRequires` comment"));
    assert!(p.contains("When unsure, write no comment,"));
    assert!(p.contains("never drop a comment another rule requires"));
    assert!(p.contains("Evidence comments may be longer"));
}

/// A BuildRequires added from a link error carries no comment of its own.
#[test]
fn a_looked_up_buildrequires_gets_no_comment() {
    let t = crate::agent::fix_after_build_text::fix_after_build_task();
    assert!(t.contains("add it without a comment"));
    assert!(!t.contains("with a comment naming the file or crate"));
    let skill = include_str!("../../../../packaging-skills/rust-cargo-packaging/SKILL.md");
    assert!(!skill.contains("with a comment naming the crate or file"));
}

/// A fixup line the edit adds always carries its comment: the prompt says so, and
/// convention 40 (the changelog and comment rules) says what the comment is.
#[test]
fn a_fixup_line_always_carries_its_comment() {
    let p = static_system_prompt();
    assert!(p.contains("is the exception: it always gets a `#` comment directly above"));
    let c = crate::conventions_table::CONVENTIONS[39].1;
    assert!(c.contains("always gets a `#` comment"), "{c}");
    assert!(c.contains("the pipeline rejects the edit"), "{c}");
}
