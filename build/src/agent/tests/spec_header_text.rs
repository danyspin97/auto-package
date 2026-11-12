use super::*;

#[test]
fn the_system_prompt_says_the_header_is_added_and_must_stay() {
    let p = static_system_prompt();
    assert!(p.contains("added by auto-package when a spec has none"));
    assert!(p.contains("Never remove it"));
}
