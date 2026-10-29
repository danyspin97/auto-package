use super::*;

#[test]
fn a_project_named_timeout_is_absent_not_a_timeout() {
    assert!(lookup_says_absent("unknown project 'timeout'", false));
    assert!(!text_is_transient("unknown project 'timeout'"));
}

#[test]
fn a_package_named_timeout_is_not_transient() {
    assert!(!text_is_transient("package 'timeout' not found"));
}

#[test]
fn a_timeout_word_without_status_is_transient() {
    assert!(text_is_transient("Operation timed out"));
    assert!(text_is_transient("connection timeout"));
    assert!(text_is_transient("Error: Timeout"));
    assert!(text_is_transient("request timeout: retry later"));
}

#[test]
fn a_timeout_inside_a_name_is_not_a_timeout() {
    assert!(!text_is_transient("cannot read home:alice:timeout"));
    assert!(!text_is_transient("no module python-timeout-decorator"));
    assert!(!text_is_transient("unknown option --timeouts"));
}

#[test]
fn a_plain_not_found_phrase_wins_over_a_timeout_word() {
    assert!(!text_is_transient("timeout: project not found"));
}

#[test]
fn a_429_is_transient_in_either_wording() {
    assert!(text_is_transient("rate limited (429)"));
    assert!(text_is_transient("HTTP Error 429: Too Many Requests"));
    assert!(text_is_transient("429 Client Error: Too Many Requests"));
    assert!(!lookup_says_absent(
        "HTTP Error 429: Too Many Requests",
        true
    ));
}
