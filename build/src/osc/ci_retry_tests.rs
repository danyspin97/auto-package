use super::ci_retry::*;
use super::*;
use std::cell::{Cell, RefCell};

fn osc_failed(stderr: &str) -> Error {
    Error::OscFailed {
        cmd: "ci".into(),
        code: 1,
        stderr: stderr.into(),
    }
}

#[test]
fn a_server_error_is_tried_again_up_to_three_times_with_backoff() {
    let tries = Cell::new(0);
    let waits = RefCell::new(Vec::new());
    let result = with_ci_retries(
        || {
            tries.set(tries.get() + 1);
            Err(osc_failed("HTTP Error 503: Service Unavailable"))
        },
        |d| waits.borrow_mut().push(d.as_secs()),
    );
    assert!(result.is_err());
    assert_eq!(tries.get(), 3);
    assert_eq!(*waits.borrow(), [5, 15]);
}

#[test]
fn a_transient_error_then_success_commits() {
    let tries = Cell::new(0);
    let result = with_ci_retries(
        || {
            tries.set(tries.get() + 1);
            if tries.get() < 2 {
                Err(osc_failed("HTTP Error 502: Bad Gateway"))
            } else {
                Ok(())
            }
        },
        |_| {},
    );
    assert!(result.is_ok());
    assert_eq!(tries.get(), 2);
}

#[test]
fn a_client_error_is_not_tried_again() {
    for text in [
        "HTTP Error 403: Forbidden",
        "HTTP Error 404: Not Found: timed out waiting for package",
        "HTTP Error 400: Bad Request",
    ] {
        let tries = Cell::new(0);
        let result = with_ci_retries(
            || {
                tries.set(tries.get() + 1);
                Err(osc_failed(text))
            },
            |_| panic!("no wait for a client error: {text}"),
        );
        assert!(result.is_err(), "{text}");
        assert_eq!(tries.get(), 1, "{text}");
    }
}

#[test]
fn a_refusal_without_a_transient_word_is_not_tried_again() {
    let tries = Cell::new(0);
    let result = with_ci_retries(
        || {
            tries.set(tries.get() + 1);
            Err(osc_failed("someone else's lock on the package"))
        },
        |_| panic!("no wait"),
    );
    assert!(result.is_err());
    assert_eq!(tries.get(), 1);
}

#[test]
fn a_server_error_and_a_timeout_are_transient_but_not_a_refusal() {
    assert!(ci_error_is_transient(&osc_failed(
        "HTTP Error 503: Service Unavailable"
    )));
    assert!(ci_error_is_transient(&osc_failed(
        "HTTP Error 504: Gateway Timeout"
    )));
    assert!(ci_error_is_transient(&osc_failed(
        "HTTP Error 429: Too Many Requests"
    )));
    assert!(ci_error_is_transient(&osc_failed(
        "connection reset by peer"
    )));
    assert!(!ci_error_is_transient(&osc_failed(
        "HTTP Error 403: Forbidden"
    )));
}
