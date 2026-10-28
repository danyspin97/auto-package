use super::*;

#[test]
fn private_and_local_addresses_are_forbidden() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "fe80::1",
        "fd00::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "240.0.0.1",
        "255.255.255.254",
        "198.18.0.1",
        "198.19.255.255",
        "192.0.0.8",
        "64:ff9b::7f00:1",
        "64:ff9b::a9fe:a9fe",
        "2002:7f00:1::",
        "2002:c0a8:101::1",
        "::127.0.0.1",
        "::10.0.0.1",
    ] {
        assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "1.1.1.1",
        "140.82.112.5",
        "2606:4700::1111",
        "198.20.0.1",
        "192.0.2.1",
        "64:ff9b::808:808",
        "2002:808:808::1",
    ] {
        assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
    }
}

#[test]
fn literal_hosts_are_judged_including_odd_numeric_forms() {
    for host in [
        "127.0.0.1",
        "[::1]",
        "[fd00::1]",
        "2130706433",
        "0x7f.1",
        "10.1",
    ] {
        assert!(is_forbidden_literal(host), "{host}");
    }
    for host in [
        "example.com",
        "1.1.1.1",
        "[2606:4700::1111]",
        "a1.example.org",
    ] {
        assert!(!is_forbidden_literal(host), "{host}");
    }
}

#[test]
fn a_literal_private_target_is_refused_even_behind_a_proxy() {
    let proxy = ureq::Proxy::new("http://127.0.0.1:9").unwrap();
    let a = agent(ureq::Agent::config_builder().proxy(Some(proxy)));
    let err = a.get("https://169.254.169.254/").call().unwrap_err();
    assert!(err.to_string().contains("private or local"), "{err}");
}

fn hdr(retry_after: Option<u64>, remaining: Option<u64>, reset: Option<u64>) -> ReplyHeaders {
    ReplyHeaders {
        retry_after,
        remaining,
        reset,
    }
}

#[test]
fn backoff_doubles_and_is_capped() {
    assert_eq!(backoff(1), Duration::from_secs(1));
    assert_eq!(backoff(2), Duration::from_secs(2));
    assert_eq!(backoff(30), MAX_BACKOFF);
}

#[test]
fn jitter_stays_within_the_bound() {
    for _ in 0..200 {
        assert!(jitter(Duration::from_secs(4)) <= Duration::from_secs(4));
    }
    assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
}

#[test]
fn retry_after_is_honoured_up_to_120_seconds() {
    let none = ReplyHeaders::default();
    assert_eq!(
        judge(429, &hdr(Some(90), None, None), 1, 0),
        Verdict::Wait(Duration::from_secs(90))
    );
    assert_eq!(
        judge(503, &hdr(Some(86_400), None, None), 1, 0),
        Verdict::Wait(MAX_RETRY_WAIT)
    );
    assert_eq!(
        judge(500, &none, 2, 0),
        Verdict::Backoff(Duration::from_secs(2))
    );
    assert_eq!(judge(500, &none, ATTEMPTS, 0), Verdict::Final);
    assert_eq!(judge(404, &none, 1, 0), Verdict::Final);
    assert_eq!(judge(403, &none, 1, 0), Verdict::Final);
}

#[test]
fn a_403_with_an_empty_quota_waits_for_a_near_reset_only() {
    // Resets in 10s: wait (plus one second of slack) and try again.
    assert_eq!(
        judge(403, &hdr(None, Some(0), Some(1_010)), 1, 1_000),
        Verdict::Wait(Duration::from_secs(11))
    );
    // GitHub's unauthenticated hour is beyond the bound: final.
    assert_eq!(
        judge(403, &hdr(None, Some(0), Some(4_600)), 1, 1_000),
        Verdict::Final
    );
    // Quota left: an ordinary refusal.
    assert_eq!(
        judge(403, &hdr(None, Some(5), Some(1_010)), 1, 1_000),
        Verdict::Final
    );
    // 429 with a reset but no Retry-After.
    assert_eq!(
        judge(429, &hdr(None, None, Some(1_005)), 1, 1_000),
        Verdict::Wait(Duration::from_secs(6))
    );
}

#[test]
fn rate_limit_detection() {
    let f = |status, h| StatusFailure { status, headers: h };
    assert!(f(429, ReplyHeaders::default()).is_rate_limit());
    assert!(f(403, hdr(None, Some(0), None)).is_rate_limit());
    assert!(!f(403, hdr(None, Some(3), None)).is_rate_limit());
    assert!(!f(500, ReplyHeaders::default()).is_rate_limit());
}

#[test]
fn the_loop_retries_with_the_injected_clock_and_gives_up_after_the_attempts() {
    let mut replies = vec![
        Outcome::Status(429, hdr(Some(7), None, None)),
        Outcome::Status(403, hdr(None, Some(0), Some(1_003))),
        Outcome::Done("ok"),
    ]
    .into_iter();
    let mut slept = Vec::new();
    let got = retry_loop(
        || replies.next().unwrap(),
        || 1_000,
        |max| max,
        |d| {
            slept.push(d);
            false
        },
    );
    assert_eq!(got.unwrap(), "ok");
    assert_eq!(slept, [Duration::from_secs(7), Duration::from_secs(4)]);

    let mut slept = Vec::new();
    let got: Result<(), _> = retry_loop(
        || Outcome::Status(502, ReplyHeaders::default()),
        || 0,
        |max| max / 2,
        |d| {
            slept.push(d);
            false
        },
    );
    assert!(matches!(
        got,
        Err(CallFailure::Status(StatusFailure { status: 502, .. }))
    ));
    // Two pauses, each half (the injected jitter) of the 1s and 2s backoff.
    assert_eq!(slept, [Duration::from_millis(500), Duration::from_secs(1)]);

    // A transient transport error is retried, a refused target is not.
    let mut n = 0;
    let got = retry_loop(
        || {
            n += 1;
            if n == 1 {
                Outcome::Error(ureq::Error::HostNotFound)
            } else {
                Outcome::Done(n)
            }
        },
        || 0,
        |m| m,
        |_| false,
    );
    assert_eq!(got.unwrap(), 2);
    let got: Result<(), _> = retry_loop(
        || {
            Outcome::Error(ureq::Error::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            )))
        },
        || 0,
        |m| m,
        |_| panic!("must not wait"),
    );
    assert!(matches!(got, Err(CallFailure::Transport(_))));
}

#[test]
fn a_stop_during_the_pause_ends_the_loop() {
    let got: Result<(), _> = retry_loop(
        || Outcome::Status(500, ReplyHeaders::default()),
        || 0,
        |m| m,
        |_| true,
    );
    assert!(matches!(
        got,
        Err(CallFailure::Transport(ureq::Error::Io(e))) if e.kind() == std::io::ErrorKind::Interrupted
    ));
}

#[test]
fn the_host_limiter_allows_a_burst_then_paces_per_host() {
    let l = HostLimiter::new(4.0, 8.0);
    let t0 = Instant::now();
    for _ in 0..8 {
        assert_eq!(l.reserve("a.example", t0), Duration::ZERO);
    }
    // The ninth waits a quarter second, the tenth half a second.
    assert_eq!(l.reserve("a.example", t0), Duration::from_millis(250));
    assert_eq!(l.reserve("a.example", t0), Duration::from_millis(500));
    // Another host has its own bucket.
    assert_eq!(l.reserve("b.example", t0), Duration::ZERO);
    // Time refills it: after 3s the debt of 2 tokens is paid and 8 are spare.
    let later = t0 + Duration::from_secs(3);
    for _ in 0..8 {
        assert_eq!(l.reserve("a.example", later), Duration::ZERO);
    }
    // Never more than the burst, however long it idled.
    let much_later = t0 + Duration::from_secs(3_600);
    for _ in 0..8 {
        assert_eq!(l.reserve("c.example", much_later), Duration::ZERO);
    }
    assert!(l.reserve("c.example", much_later) > Duration::ZERO);
}

#[test]
fn a_retry_pause_ends_early_when_stopped() {
    let stop = AtomicBool::new(true);
    let t = Instant::now();
    assert!(sleep_unless_stopped(Duration::from_secs(30), &stop));
    assert!(t.elapsed() < Duration::from_secs(1));
    let go = AtomicBool::new(false);
    assert!(!sleep_unless_stopped(Duration::from_millis(10), &go));
}

#[test]
fn only_transport_faults_are_transient() {
    assert!(is_transient(&ureq::Error::HostNotFound));
    assert!(!is_transient(&ureq::Error::StatusCode(500)));
    assert!(!is_transient(&ureq::Error::Io(std::io::Error::from(
        std::io::ErrorKind::PermissionDenied
    ))));
}

#[test]
fn a_loopback_target_is_refused_and_http_is_too() {
    let a = agent(ureq::Agent::config_builder().proxy(None));
    let err = a.get("https://127.0.0.1:1/").call().unwrap_err();
    assert!(err.to_string().contains("private or local"), "{err}");
    assert!(a.get("http://example.com/").call().is_err());
}
