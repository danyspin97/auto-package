use super::changes_author::*;
use std::collections::BTreeMap;

const SEP: &str = "-------------------------------------------------------------------";
const RC: &str = "[general]\napiurl = https://api.example.org\n\n[https://api.example.org]\nuser = u\nrealname = Rc Name\nemail = rc@example.org\n";

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k| map.get(k).cloned()
}

fn offline(_: &str, _: &str) -> Result<ObsProfile, String> {
    Err("offline".into())
}

#[test]
fn the_oscrc_of_the_default_api_gives_the_author() {
    let a = resolve_author(&env(&[]), RC, None, &offline).unwrap();
    assert_eq!(a.to_string(), "Rc Name <rc@example.org>");
}

#[test]
fn the_environment_wins_over_the_oscrc_and_email_comes_last() {
    let e = env(&[
        ("VC_MAILADDR", "v@example.org"),
        ("EMAIL", "E <e@example.org>"),
    ]);
    assert_eq!(
        resolve_author(&e, RC, None, &offline).unwrap().to_string(),
        "Rc Name <v@example.org>"
    );
    let only_email = env(&[("EMAIL", "Env Name <e@example.org>")]);
    assert_eq!(
        resolve_author(&only_email, "", None, &offline)
            .unwrap()
            .to_string(),
        "Env Name <e@example.org>"
    );
}

#[test]
fn a_placeholder_is_skipped_never_used() {
    let e = env(&[
        ("VC_MAILADDR", " YOUR EMAIL HERE "),
        ("VC_REALNAME", "YOUR NAME"),
    ]);
    assert_eq!(
        resolve_author(&e, RC, None, &offline).unwrap().to_string(),
        "Rc Name <rc@example.org>"
    );
    let err = resolve_author(&e, "", None, &offline).unwrap_err();
    assert!(err.contains("no usable name or email"), "{err}");
}

#[test]
fn a_bare_email_alone_has_no_name_and_the_message_says_what_to_set() {
    let err = resolve_author(&env(&[("EMAIL", "a@example.org")]), "", None, &offline).unwrap_err();
    assert!(err.contains("no usable name"), "{err}");
    assert!(
        err.contains("realname") && err.contains("VC_MAILADDR"),
        "{err}"
    );
    assert!(err.contains("api.opensuse.org"), "{err}");
}

#[test]
fn an_address_setting_may_be_name_and_address_and_quotes_are_dropped() {
    let e = env(&[("VC_MAILADDR", "Jo Doe <jo@example.org>")]);
    assert_eq!(
        resolve_author(&e, "", None, &offline).unwrap().to_string(),
        "Jo Doe <jo@example.org>"
    );
    let rc = "[general]\napiurl = https://a.example.org\n[https://a.example.org]\nEmail = \"Rc <rc@example.org>\"\nREALNAME = 'Rc Real'\n";
    assert_eq!(
        resolve_author(&env(&[]), rc, None, &offline)
            .unwrap()
            .to_string(),
        "Rc Real <rc@example.org>"
    );
    let rc = "[general]\napiurl = https://a.example.org\n[https://a.example.org]\nemail = \"rc@example.org\"\nrealname = R\n";
    assert_eq!(
        resolve_author(&env(&[]), rc, None, &offline)
            .unwrap()
            .to_string(),
        "R <rc@example.org>"
    );
}

#[test]
fn a_control_character_makes_a_name_unusable() {
    for bad in [
        "Me\rX",
        "Me\0X",
        "Me\tX",
        "Me\u{2028}X",
        "Me\u{2029}X",
        "Me\u{7f}X",
    ] {
        let pairs = [("VC_REALNAME", bad), ("VC_MAILADDR", "me@example.org")];
        let e = env(&pairs);
        assert!(resolve_author(&e, "", None, &offline).is_err(), "{bad:?}");
    }
}

#[test]
fn another_apis_section_is_not_read() {
    let err =
        resolve_author(&env(&[]), RC, Some("https://other.example.org"), &offline).unwrap_err();
    assert!(err.contains("other.example.org"), "{err}");
}

#[test]
fn only_a_name_and_an_address_parse() {
    assert!(parse_author("Me <me@example.org>").is_some());
    for bad in [
        "YOUR EMAIL HERE",
        "me@example.org",
        " <me@example.org>",
        "Me <nope>",
        "Me <YOUR EMAIL HERE>",
    ] {
        assert!(parse_author(bad).is_none(), "{bad}");
    }
}

fn who() -> Author {
    Author {
        name: "Me".into(),
        email: "me@example.org".into(),
    }
}

#[test]
fn only_an_unpublished_entry_with_a_bad_author_is_rewritten() {
    let old = format!("{SEP}\nMon Jan  1 00:00:00 UTC 2024 - YOUR EMAIL HERE\n\n- Old\n");
    let text = format!("{SEP}\nThu Nov 12 14:58:34 UTC 2026 - YOUR EMAIL HERE\n\n- New\n\n{old}");
    let fixed = with_repaired(&text, &old).unwrap();
    assert!(fixed.contains("Thu Nov 12 14:58:34 UTC 2026 - Me <me@example.org>\n"));
    assert!(fixed.ends_with(&old), "the published entry is untouched");
    assert!(with_repaired(&fixed, &old).is_none(), "idempotent");
}

#[test]
fn a_good_author_and_line_endings_are_kept() {
    let good =
        format!("{SEP}\r\nThu Nov 12 14:58:34 UTC 2026 - A B <a@example.org>\r\n\r\n- New\r\n");
    assert!(with_repaired(&good, "").is_none());
    let bad = good.replace("A B <a@example.org>", " <a@example.org>");
    let fixed = with_repaired(&bad, "").unwrap();
    assert!(fixed.contains("- Me <me@example.org>\r\n"), "{fixed:?}");
    assert_eq!(fixed.matches("\r\n").count(), bad.matches("\r\n").count());
}

fn with_repaired(text: &str, published: &str) -> Option<String> {
    super::changes_author::with_repaired_authors(text, published, &who())
}

#[test]
fn the_files_of_a_checkout_are_repaired_against_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    std::fs::create_dir(base.path().join("files")).unwrap();
    let old = format!("{SEP}\nMon Jan  1 00:00:00 UTC 2024 - YOUR EMAIL HERE\n\n- Old\n");
    std::fs::write(base.path().join("files/p.changes"), &old).unwrap();
    let text = format!("{SEP}\nThu Nov 12 14:58:34 UTC 2026 - YOUR EMAIL HERE\n\n- New\n\n{old}");
    std::fs::write(dir.path().join("p.changes"), &text).unwrap();
    let done = repair_unpublished_authors(dir.path(), Some(base.path()), &who());
    assert_eq!(done, vec!["p.changes".to_string()]);
    let after = std::fs::read_to_string(dir.path().join("p.changes")).unwrap();
    assert_eq!(after.matches("YOUR EMAIL HERE").count(), 1);
    assert!(repair_unpublished_authors(dir.path(), Some(base.path()), &who()).is_empty());
}

const RC_USER: &str =
    "[general]\napiurl = https://api.example.org\n[https://api.example.org]\nuser = jdoe\n";

fn profile(
    realname: Option<&str>,
    email: Option<&str>,
) -> impl Fn(&str, &str) -> Result<ObsProfile, String> {
    let (r, e) = (realname.map(String::from), email.map(String::from));
    move |apiurl, user| {
        assert_eq!((apiurl, user), ("https://api.example.org", "jdoe"));
        Ok(ObsProfile {
            realname: r.clone(),
            email: e.clone(),
        })
    }
}

#[test]
fn the_obs_profile_serves_what_the_other_sources_lack() {
    let p = profile(Some("Prof Name"), Some("prof@example.org"));
    assert_eq!(
        resolve_author(&env(&[]), RC_USER, None, &p)
            .unwrap()
            .to_string(),
        "Prof Name <prof@example.org>"
    );
    let e = env(&[("VC_REALNAME", "Env Name")]);
    assert_eq!(
        resolve_author(&e, RC_USER, None, &p).unwrap().to_string(),
        "Env Name <prof@example.org>"
    );
}

#[test]
fn the_profile_is_not_asked_when_the_other_sources_suffice() {
    let e = env(&[("VC_REALNAME", "A"), ("VC_MAILADDR", "a@example.org")]);
    let never = |_: &str, _: &str| -> Result<ObsProfile, String> { panic!("asked") };
    assert!(resolve_author(&e, RC_USER, None, &never).is_ok());
}

#[test]
fn an_unreadable_profile_is_named_in_the_stop_message_and_says_what_was_found() {
    let e = env(&[("VC_MAILADDR", "a@example.org")]);
    let err = resolve_author(&e, RC_USER, None, &offline).unwrap_err();
    assert!(
        err.contains("no usable name (found a@example.org in VC_MAILADDR)"),
        "{err}"
    );
    assert!(
        err.contains("could not read your OBS profile: offline"),
        "{err}"
    );
    assert!(err.contains("realname"), "{err}");
    let err = resolve_author(&e, "", None, &offline).unwrap_err();
    assert!(err.contains("names no `user`"), "{err}");
    let empty = |_: &str, _: &str| Ok(ObsProfile::default());
    let err = resolve_author(&e, RC_USER, None, &empty).unwrap_err();
    assert!(!err.contains("could not read"), "{err}");
}

#[test]
fn a_checkout_resolves_against_its_own_api_section() {
    let rc = "[general]\napiurl = https://api.example.org\n\
        [https://api.example.org]\nrealname = Default Name\nemail = d@example.org\n\
        [https://api.other.org]\nrealname = Other Name\nemail = o@example.org\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".osc")).unwrap();
    std::fs::write(dir.path().join(".osc/_apiurl"), "https://api.other.org\n").unwrap();
    let who = |c: Option<&std::path::Path>| {
        resolve_author_for(c, &env(&[]), rc, &offline)
            .unwrap()
            .to_string()
    };
    assert_eq!(who(Some(dir.path())), "Other Name <o@example.org>");
    assert_eq!(who(None), "Default Name <d@example.org>");
    let bare = tempfile::tempdir().unwrap();
    assert_eq!(who(Some(bare.path())), "Default Name <d@example.org>");
}
