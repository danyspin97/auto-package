use super::*;
use std::path::{Path, PathBuf};

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

// ── the user's file ──────────────────────────────────────────────────────────

use super::file::{self, Added};

#[test]
fn the_file_takes_comments_reasons_and_one_host_per_line() {
    let text = "# a comment\n\n   # indented comment\n\
                mirror.example.org  # our package mirror\n\
                Other.Example.NET.\n\
                bare.example.com\n";
    let parsed = file::parse(text);
    assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
    let got: Vec<(&str, Option<&str>)> = parsed
        .entries
        .iter()
        .map(|e| (e.host.as_str(), e.reason.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("mirror.example.org", Some("our package mirror")),
            ("other.example.net", None),
            ("bare.example.com", None),
        ]
    );
    assert!(parsed.entries.iter().all(|e| e.origin == Origin::File));
}

#[test]
fn malformed_lines_are_reported_with_their_number_and_ignored() {
    let text = "good.example.org\n\
                https://bad.example.org\n\
                two hosts.example.org\n\
                10.0.0.1 # an address\n\
                *.wild.example.org\n\
                ok2.example.org:443\n\
                good.example.org # again\n";
    let parsed = file::parse(text);
    let hosts: Vec<&str> = parsed.entries.iter().map(|e| e.host.as_str()).collect();
    assert_eq!(hosts, ["good.example.org"], "a repeat keeps the first line");
    let lines: Vec<&str> = parsed
        .problems
        .iter()
        .map(|p| p.split(':').next().unwrap())
        .collect();
    assert_eq!(lines, ["line 2", "line 3", "line 4", "line 5", "line 6"]);
}

#[cfg(unix)]
#[test]
fn the_file_and_its_directory_are_private_and_round_trip() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("auto-package");
    let path = dir.join(file::FILE_NAME);
    assert!(file::load(&path).entries.is_empty());
    assert_eq!(
        file::add_host(&path, "mirror.example.org", Some("mirror\nwith a newline")).unwrap(),
        Added::New
    );
    assert_eq!(
        file::add_host(&path, "MIRROR.example.org.", None).unwrap(),
        Added::AlreadyPresent
    );
    assert_eq!(
        file::add_host(&path, "10.0.0.1", None)
            .unwrap_err()
            .contains("IP address"),
        true
    );
    file::add_host(&path, "second.example.org", None).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "mirror.example.org  # mirror with a newline\nsecond.example.org\n"
    );
    let loaded = file::load(&path);
    assert!(loaded.problems.is_empty());
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(
        loaded.entries[0].reason.as_deref(),
        Some("mirror with a newline")
    );
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir), 0o700);
    // The atomic write leaves no temporary file behind; the lock file stays.
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [file::FILE_NAME, file::LOCK_NAME]);
}

#[cfg(unix)]
#[test]
fn a_symlinked_list_is_written_through_and_stays_a_link() {
    let root = tempfile::tempdir().unwrap();
    let real_dir = root.path().join("dotfiles");
    std::fs::create_dir(&real_dir).unwrap();
    let real = real_dir.join(file::FILE_NAME);
    std::fs::write(&real, "keep.example.org\n").unwrap();
    let dir = root.path().join("auto-package");
    std::fs::create_dir(&dir).unwrap();
    let link = dir.join(file::FILE_NAME);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    file::add_host(&link, "new.example.org", None).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "keep.example.org\nnew.example.org\n"
    );
    assert!(file::remove_host(&link, "keep.example.org").unwrap());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new.example.org\n");
}

#[cfg(unix)]
#[test]
fn a_link_to_another_directory_changes_neither_that_directory_nor_gets_a_lock() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let real_dir = root.path().join("dotfiles");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::set_permissions(&real_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let real = real_dir.join(file::FILE_NAME);
    std::fs::write(&real, "keep.example.org\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    let dir = root.path().join("auto-package");
    std::fs::create_dir(&dir).unwrap();
    let link = dir.join(file::FILE_NAME);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    file::add_host(&link, "new.example.org", None).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&real_dir),
        0o755,
        "the target's directory is not chmodded"
    );
    assert_eq!(mode(&real), 0o640, "an existing target keeps its mode");
    assert!(
        !real_dir.join(file::LOCK_NAME).exists(),
        "no lock next to the target"
    );
    assert!(
        dir.join(file::LOCK_NAME).exists(),
        "the lock is next to the link"
    );
    assert_eq!(mode(&dir), 0o700);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_link_is_written_to_its_target_as_a_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let real_dir = root.path().join("dotfiles");
    std::fs::create_dir(&real_dir).unwrap();
    let real = real_dir.join(file::FILE_NAME);
    let dir = root.path().join("auto-package");
    std::fs::create_dir(&dir).unwrap();
    let link = dir.join(file::FILE_NAME);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        file::add_host(&link, "new.example.org", None),
        Ok(file::Added::New)
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new.example.org\n");
    let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn a_link_to_a_directory_fails_without_changing_the_directory() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = root.path().join("auto-package");
    std::fs::create_dir(&dir).unwrap();
    let link = dir.join(file::FILE_NAME);
    std::os::unix::fs::symlink(&other, &link).unwrap();
    assert!(file::add_host(&link, "new.example.org", None).is_err());
    let mode = std::fs::metadata(&other).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    assert!(!other.join(file::LOCK_NAME).exists());
}

#[test]
fn control_characters_are_not_kept_in_a_reason_or_a_line() {
    let parsed = file::parse("mirror.example.org  # \x1b[31mred\x1b[0m\tnote\n");
    assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
    let reason = parsed.entries[0].reason.clone().unwrap();
    assert!(!reason.chars().any(char::is_control), "{reason:?}");
    assert!(reason.contains("note"), "{reason:?}");
}

#[test]
fn a_file_line_gets_the_user_name_rule() {
    let parsed = file::parse("com\nco.uk\nok.example.org\n");
    let hosts: Vec<&str> = parsed.entries.iter().map(|e| e.host.as_str()).collect();
    assert_eq!(hosts, ["ok.example.org"]);
    assert_eq!(parsed.problems.len(), 2, "{:?}", parsed.problems);
}

#[test]
fn removing_a_host_keeps_the_other_lines() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("allowed-hosts");
    std::fs::write(&path, "# keep me\na.example.org  # one\nb.example.org\n").unwrap();
    assert!(file::remove_host(&path, "A.example.org").unwrap());
    assert!(!file::remove_host(&path, "a.example.org").unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# keep me\nb.example.org\n"
    );
    // Removing from a missing file changes nothing and creates nothing.
    let missing = root.path().join("nope");
    assert!(!file::remove_host(&missing, "a.example.org").unwrap());
    assert!(!missing.exists());
}

#[test]
fn the_config_directory_follows_xdg_then_home() {
    use std::ffi::OsString;
    let s = |v: &str| Some(OsString::from(v));
    assert_eq!(
        file::config_dir_in(s("/x/cfg"), s("/home/u")),
        Some(PathBuf::from("/x/cfg/auto-package"))
    );
    // A relative XDG_CONFIG_HOME is ignored, as the spec says.
    assert_eq!(
        file::config_dir_in(s("rel"), s("/home/u")),
        Some(PathBuf::from("/home/u/.config/auto-package"))
    );
    assert_eq!(file::config_dir_in(None, None), None);
    assert_eq!(file::config_dir_in(None, s("")), None);
}
