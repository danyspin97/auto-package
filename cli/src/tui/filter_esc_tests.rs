use super::*;

#[test]
fn esc_dismisses_the_prompt_then_the_filter_then_the_entry_then_quits() {
    let mut browse = BrowseState {
        act_expanded: true,
        filter: Some("jq".to_string()),
        filter_input: Some(String::new()),
        ..Default::default()
    };
    assert_eq!(esc_action(&browse), EscAction::ClosePrompt);
    browse.filter_input = None;
    assert_eq!(esc_action(&browse), EscAction::ClearFilter);
    browse.filter = None;
    assert_eq!(esc_action(&browse), EscAction::Collapse);
    browse.act_expanded = false;
    assert_eq!(esc_action(&browse), EscAction::Quit);
}

#[test]
fn selecting_another_package_drops_its_filter() {
    let mut browse = BrowseState {
        filter: Some("jq".to_string()),
        filter_input: Some("j".to_string()),
        ..Default::default()
    };
    browse.reset_for_package(0);
    assert!(browse.filter.is_none());
    assert!(browse.filter_input.is_none());
}
