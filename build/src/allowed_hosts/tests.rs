use super::*;

fn test_asker() -> Asker {
    Asker::service("test-pkg", "test")
}

#[test]
fn validator_accepts_plain_names_and_normalizes_case_and_one_dot() {
    for (raw, want) in [
        ("github.com", "github.com"),
        ("GitHub.COM", "github.com"),
        ("github.com.", "github.com"),
        ("a-b.example.org", "a-b.example.org"),
        ("x1.y2", "x1.y2"),
    ] {
        assert_eq!(normalize_host(raw).as_deref(), Ok(want), "{raw}");
    }
}

#[test]
fn validator_rejects_everything_that_is_not_a_plain_name() {
    for raw in [
        "",
        ".",
        "github.com..",
        "1.2.3.4",
        "127.0.0.1",
        "[::1]",
        "::1",
        "2130706433",
        "0x7f.1",
        "example.com:443",
        "example.com:",
        "https://example.com",
        "example.com/path",
        "user@example.com",
        "exa mple.com",
        "-bad.example.com",
        "bad-.example.com",
        "a..b",
        "*.example.org",
        "example.*",
        "münchen.de",
        "xn--a.example",
        "xn--.example",
        "example.com\n",
        "example.com?x",
    ] {
        assert!(normalize_host(raw).is_err(), "{raw:?} should be refused");
    }
}

#[test]
fn validator_refuses_long_names_and_labels() {
    let label63 = format!("{}.example.org", "a".repeat(63));
    assert!(normalize_host(&label63).is_ok());
    let label64 = format!("{}.example.org", "a".repeat(64));
    assert!(normalize_host(&label64).is_err());
    let long = format!("{}.org", vec!["abcdefgh"; 30].join("."));
    assert!(long.len() > 253);
    assert!(normalize_host(&long).is_err());
}

#[test]
fn wildcards_say_they_are_not_supported() {
    let err = normalize_host("*.example.org").unwrap_err();
    assert!(err.contains("wildcards are not supported"), "{err}");
}

#[test]
fn builtin_hosts_come_first_then_env_file_and_run_in_order() {
    let h = AllowedHosts::with_env("mirror.example.org");
    h.allow("mirror.example.org", Origin::File, None).unwrap();
    // The env entry was first: a second origin does not replace it.
    assert_eq!(
        h.origin_of("mirror.example.org", Use::Service),
        Some(Origin::Env)
    );
    assert_eq!(
        h.origin_of("github.com", Use::Service),
        Some(Origin::Builtin)
    );
    assert_eq!(
        h.allow("github.com", Origin::Run(Via::Flag), None).unwrap(),
        AllowOutcome::AlreadyAllowed(Origin::Builtin)
    );
    h.allow("other.example.net", Origin::Run(Via::Flag), None)
        .unwrap();
    h.allow("file.example.net", Origin::File, Some("why".into()))
        .unwrap();
    h.allow("tui.example.net", Origin::Run(Via::Tui), None)
        .unwrap();
    let order: Vec<Origin> = h
        .entries()
        .into_iter()
        .filter(|e| e.host.ends_with("example.net") || e.host == "mirror.example.org")
        .map(|e| e.origin)
        .collect();
    assert_eq!(
        order,
        [
            Origin::Env,
            Origin::Run(Via::Flag),
            Origin::File,
            Origin::Run(Via::Tui)
        ]
    );
}

#[test]
fn builtin_lists_differ_for_services_and_sources() {
    let h = AllowedHosts::empty();
    // A service-only host is refused as a source.
    assert!(h.allows("proxy.golang.org", Use::Service));
    assert!(!h.allows("proxy.golang.org", Use::Source));
    // Source indexes are also reachable by services, as before.
    assert!(h.allows("rubygems.org", Use::Source));
    assert!(h.allows("rubygems.org", Use::Service));
    // The forges are on both.
    assert!(h.allows("github.com", Use::Source));
    assert!(h.allows("sub.gitlab.com", Use::Service));
}

#[test]
fn user_entries_cover_subdomains_and_nothing_else() {
    let h = AllowedHosts::empty();
    h.allow("mirror.example.org", Origin::File, None).unwrap();
    assert!(h.allows("mirror.example.org", Use::Service));
    assert!(h.allows("a.mirror.example.org", Use::Source));
    assert!(!h.allows("example.org", Use::Service));
    assert!(!h.allows("mirror.example.org.evil", Use::Service));
}

#[test]
fn refusal_names_the_host_and_says_how_to_allow_it() {
    let h = AllowedHosts::empty();
    let err = h
        .check_host("example.org", Use::Service, &test_asker())
        .unwrap_err();
    assert_eq!(err, refusal_hint("example.org", Use::Service));
    assert!(
        err.contains(
            "host example.org is not allowed; allow it for this run with --allow-host example.org"
        ),
        "{err}"
    );
    assert!(
        err.contains("`auto-package hosts allow example.org`"),
        "{err}"
    );
    assert_eq!(
        h.refused_hosts(),
        [("example.org".to_string(), Use::Service)]
    );
    // An invalid name is refused without a hint to allow it.
    let bad = h
        .check_host("1.2.3.4", Use::Service, &test_asker())
        .unwrap_err();
    assert!(!bad.contains("--allow-host"), "{bad}");
    assert_eq!(
        h.refused_hosts(),
        [("example.org".to_string(), Use::Service)]
    );
}

#[test]
fn a_refusal_keeps_the_package_and_the_request_that_caused_it() {
    let h = AllowedHosts::empty();
    let awww = Asker::service("awww", "cargo_vendor");
    let jay = Asker::source("jay");
    for asker in [&awww, &jay, &awww] {
        assert!(
            h.check_host("evil.example.net", Use::Service, asker)
                .is_err()
        );
    }
    let refusals = h.refusals();
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    assert!(refusals.contains(&("evil.example.net".into(), Use::Service, awww.clone())));
    assert!(refusals.contains(&("evil.example.net".into(), Use::Service, jay.clone())));
    // The per-host view still names each host once.
    assert_eq!(
        h.refused_hosts(),
        [("evil.example.net".to_string(), Use::Service)]
    );
    assert_eq!(awww.what, "service cargo_vendor");
    assert_eq!(jay.what, "source lookup");
}

#[test]
fn refused_hosts_are_read_back_from_a_failure_text_once_each() {
    let text = format!(
        "service failed\nsandbox proxy refused: 403 a.example:443: {}\n\
         sandbox proxy refused: 403 b.example:443: {}\n\
         sandbox proxy refused: 403 a.example:443: {}",
        refusal_hint("a.example", Use::Service),
        refusal_hint("b.example", Use::Service),
        refusal_hint("a.example", Use::Service),
    );
    assert_eq!(refused_hosts_in(&text), ["a.example", "b.example"]);
    assert!(refused_hosts_in("no refusal here, only --allow-host x").is_empty());
}

#[test]
fn a_host_allowed_at_runtime_is_seen_by_the_same_object() {
    let h = std::sync::Arc::new(AllowedHosts::empty());
    let reader = std::sync::Arc::clone(&h);
    assert!(
        reader
            .check_host("late.example.org", Use::Service, &test_asker())
            .is_err()
    );
    h.allow("late.example.org", Origin::Run(Via::Tui), None)
        .unwrap();
    assert_eq!(
        reader
            .check_host("late.example.org", Use::Service, &test_asker())
            .as_deref(),
        Ok("late.example.org")
    );
}

#[test]
fn bad_env_names_are_reported_and_skipped() {
    let h = AllowedHosts::with_env("ok.example.org, 10.0.0.1 ,*.wild.example,, https://x.example");
    assert!(h.allows("ok.example.org", Use::Service));
    assert!(!h.allows("10.0.0.1", Use::Service));
    assert_eq!(h.problems().len(), 3, "{:?}", h.problems());
    assert!(h.problems()[0].starts_with("AUTO_PACKAGE_SOURCE_HOSTS:"));
}

#[test]
fn punycode_labels_are_accepted_only_when_they_decode_to_a_name() {
    // `xn--mnchen-3ya` is the ASCII form of münchen. `xn--a` decodes to a
    // control character, which is not a name, so it is refused.
    assert_eq!(
        normalize_host("XN--MNCHEN-3YA.de").as_deref(),
        Ok("xn--mnchen-3ya.de")
    );
    assert!(normalize_host("xn--a.example").is_err());
    assert!(normalize_host("xn--.example").is_err());
    assert!(normalize_host("xn--mnchen-3ya_.de").is_err());
}

#[test]
fn a_user_may_not_allow_a_single_label_or_a_public_suffix() {
    for raw in ["com", "co.uk", "CO.UK.", "github.io", "com.au"] {
        assert!(normalize_user_host(raw).is_err(), "{raw} should be refused");
    }
    assert_eq!(
        normalize_user_host("mirror.example.co.uk").as_deref(),
        Ok("mirror.example.co.uk")
    );
    assert!(!AllowedHosts::empty().allows("example.com", Use::Service));
    let env = AllowedHosts::with_env("com, mirror.example.org");
    assert!(env.allows("mirror.example.org", Use::Service));
    assert!(!env.allows("example.com", Use::Service));
    assert_eq!(env.problems().len(), 1, "{:?}", env.problems());
}

#[test]
fn the_suffix_list_refuses_hosting_names_and_aws_shared_names() {
    // Suffixes the list must hold (each is in the public suffix list). The
    // hosting names are refused as exact names only: a customer's name under
    // them is allowed.
    for raw in [
        "workers.dev",
        "web.app",
        "fly.dev",
        "ngrok.io",
        "readthedocs.io",
        "githubusercontent.com",
        "co.za",
        "com.mx",
        "com.ua",
        "ac.in",
        "amazonaws.com",
        "s3.amazonaws.com",
        "s3.us-east-1.amazonaws.com",
        "bucket.s3.amazonaws.com",
    ] {
        assert!(normalize_user_host(raw).is_err(), "{raw} should be refused");
    }
    for raw in [
        "foo.github.io",
        "me.workers.dev",
        "mirror.s3.eu-west-1.amazonaws.com",
        "bucket.s3.us-east-1.amazonaws.com",
        "mirror.example.com.ua",
    ] {
        assert_eq!(normalize_user_host(raw).as_deref(), Ok(raw), "{raw}");
    }
}

#[test]
fn a_host_built_in_for_services_only_can_be_allowed_for_sources() {
    let h = AllowedHosts::empty();
    assert!(
        h.check_host("proxy.golang.org", Use::Source, &test_asker())
            .is_err()
    );
    assert_eq!(h.origin_of("proxy.golang.org", Use::Source), None);
    assert_eq!(
        h.allow("proxy.golang.org", Origin::Run(Via::Flag), None),
        Ok(AllowOutcome::Added)
    );
    assert_eq!(
        h.check_host("proxy.golang.org", Use::Source, &test_asker())
            .as_deref(),
        Ok("proxy.golang.org")
    );
    // A host built in for both uses is already allowed; nothing is added.
    assert_eq!(
        h.allow("github.com", Origin::Run(Via::Flag), None),
        Ok(AllowOutcome::AlreadyAllowed(Origin::Builtin))
    );
}
