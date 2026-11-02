//! Which failures of a fetch are tried again, and how long the pause is: a 429
//! waits for what the server asks, a 404 or 410 is the upstream's, and a body
//! cut by the network is fetched again. No network: the fakes below serve it.

use std::cell::RefCell;
use std::io::Read;
use std::time::Duration;

use super::fetch::{
    Dep, Fetch, RATE_LIMIT_PAUSE, Refusal, Request, Response, fetch_vendor, rate_limited,
    status_refusal,
};
use super::fetch_git::NoGit;
use super::fetch_tests::{Quiet, dep, root};
use crate::pipeline::FailureKind;

const PIXMAN: &[u8] = include_bytes!("testdata/pixman-0.3.0.tar.gz");
const PIXMAN_URL: &str = "https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz";
const PIXMAN_HASH: &str = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";

/// A body that the network cuts: every read fails as the connection resets.
struct Reset;

impl Read for Reset {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
    }
}

/// Serves the pixman archive, its body cut for the first `cuts` requests.
struct Cut {
    cuts: usize,
    calls: RefCell<usize>,
}

impl Fetch for Cut {
    fn get(&self, _url: &str) -> Result<Response, Refusal> {
        let asked = {
            let mut calls = self.calls.borrow_mut();
            *calls += 1;
            *calls
        };
        let body: Box<dyn Read> = if asked <= self.cuts {
            Box::new(Reset)
        } else {
            Box::new(std::io::Cursor::new(PIXMAN))
        };
        Ok(Response::Body {
            content_type: Some("application/octet-stream".to_string()),
            body,
        })
    }
}

fn run(fetch: &dyn Fetch) -> Result<Vec<Dep>, Refusal> {
    crate::workspace::disable_disk_gate_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let req = Request {
        package: "pkg",
        fetch,
        git: &NoGit,
        work: dir.path(),
        limits: super::unpack::LIMITS,
        reporter: &Quiet,
        pause: Duration::ZERO,
    };
    fetch_vendor(&req, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)))
}

#[test]
fn a_429_waits_for_the_retry_after_the_server_names() {
    let mut headers = ureq::http::HeaderMap::new();
    headers.insert(ureq::http::header::RETRY_AFTER, "7".parse().unwrap());
    let got = rate_limited(PIXMAN_URL, &headers);
    assert_eq!(got.kind, FailureKind::Transient);
    assert_eq!(got.wait, Some(Duration::from_secs(7)));
}

#[test]
fn a_retry_after_longer_than_a_minute_is_cut_to_a_minute() {
    let mut headers = ureq::http::HeaderMap::new();
    headers.insert(ureq::http::header::RETRY_AFTER, "3600".parse().unwrap());
    let got = rate_limited(PIXMAN_URL, &headers);
    assert_eq!(got.wait, Some(Duration::from_secs(60)));
}

#[test]
fn a_429_with_no_retry_after_waits_the_default_pause() {
    let got = rate_limited(PIXMAN_URL, &ureq::http::HeaderMap::new());
    assert_eq!(got.kind, FailureKind::Transient);
    assert_eq!(got.wait, Some(RATE_LIMIT_PAUSE));
}

#[test]
fn a_gone_url_is_the_upstreams_and_a_server_error_is_retried() {
    assert_eq!(
        status_refusal(PIXMAN_URL, 404).kind,
        FailureKind::BrokenOutside
    );
    assert_eq!(
        status_refusal(PIXMAN_URL, 410).kind,
        FailureKind::BrokenOutside
    );
    assert_eq!(status_refusal(PIXMAN_URL, 503).kind, FailureKind::Transient);
    assert_eq!(status_refusal(PIXMAN_URL, 418).kind, FailureKind::Other);
}

#[test]
fn a_body_cut_once_by_the_network_is_fetched_again() {
    let cut = Cut {
        cuts: 1,
        calls: RefCell::new(0),
    };
    let got = run(&cut);
    assert!(got.is_ok(), "{:?}", got.err());
    assert_eq!(*cut.calls.borrow(), 2);
}

#[test]
fn a_body_cut_every_time_ends_as_transient_after_three_tries() {
    let cut = Cut {
        cuts: usize::MAX,
        calls: RefCell::new(0),
    };
    let err = run(&cut).unwrap_err();
    assert_eq!(err.kind, FailureKind::Transient, "{}", err.msg);
    assert_eq!(*cut.calls.borrow(), 3);
}
