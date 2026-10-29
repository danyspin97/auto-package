use super::*;

#[test]
fn recognizes_osc_http_not_found_wording() {
    assert!(is_not_found("urllib: HTTP Error 404: Not Found\n"));
    assert!(is_not_found(
        "Server returned an error: HTTP Error 404: Not Found\n"
    ));
}

#[test]
fn a_not_found_phrase_without_the_http_wording_is_not_a_404() {
    assert!(!is_not_found("404 Client Error: Not Found for url"));
    assert!(!is_not_found("config file Not Found, using defaults"));
    assert!(!is_not_found(
        "HTTP Error 500: Internal Server Error: Not Found in cache"
    ));
}

#[test]
fn a_bare_number_containing_404_is_not_a_missing_resource() {
    assert!(!is_not_found("request id 1404 timed out"));
    assert!(!is_not_found("Skipping foo: conflict"));
}

/// `ensure_package` creates a package only when `osc ls` says 404; a
/// network, auth or server failure must not read as "missing".
#[test]
fn other_failures_are_not_a_missing_package() {
    assert!(!is_not_found(
        "Server returned an error: HTTP Error 500: Internal Server Error"
    ));
    assert!(!is_not_found("Name or service not known"));
    assert!(!is_not_found("HTTP Error 401: Unauthorized"));
}
