use super::*;
use crate::allowed_hosts::AllowedHosts;
use std::cell::RefCell;

const BUG: &str = include_str!("../testdata/bugfix/bsc1234567_bug.json");
const COMMENTS: &str = include_str!("../testdata/bugfix/bsc1234567_comments.json");
const PRIVATE: &str = include_str!("../testdata/bugfix/bug_private.json");

#[test]
fn a_bug_is_read_by_its_number() {
    assert_eq!(bug_number("bsc#1234567"), Some(1234567));
    assert_eq!(bug_number("boo#1234567"), Some(1234567));
    assert_eq!(bug_number("jsc#SLE-1234"), None);
    assert_eq!(bug_number("CVE-2025-1234"), None);
    let [a, b] = bug_paths(1234567);
    assert!(a.starts_with("https://bugzilla.suse.com/rest/bug/1234567?include_fields="));
    assert_eq!(b, "https://bugzilla.suse.com/rest/bug/1234567/comment");
}

#[test]
fn the_report_and_the_last_five_comments_are_kept() {
    let t = parse_bug(BUG, COMMENTS, 1234567).unwrap();
    assert!(t.summary.contains("heap overflow"));
    assert_eq!(t.status, "NEW");
    assert_eq!(t.urls.len(), 3);
    // The report, then the last five public comments; the private one is out.
    assert_eq!(t.comments.len(), 6);
    assert!(t.comments[0].starts_with("A heap overflow"));
    assert_eq!(t.comments[1], "c3");
    assert_eq!(t.comments[5], "c7");
    assert!(!t.render().contains("internal note"));
}

#[test]
fn a_long_bug_is_cut() {
    let big = "x".repeat(100_000);
    let comments = format!(
        r#"{{"bugs":{{"7":{{"comments":[{{"text":"{big}"}},{{"text":"{big}"}},{{"text":"{big}"}},{{"text":"{big}"}},{{"text":"{big}"}},{{"text":"{big}"}}]}}}}}}"#
    );
    let t = parse_bug(r#"{"bugs":[{"id":7}]}"#, &comments, 7).unwrap();
    let total: usize = t.comments.iter().map(String::len).sum();
    assert!(total < 21 * 1024 + 6 * 64, "{total}");
}

#[test]
fn a_private_bug_is_not_public() {
    assert_eq!(parse_bug(PRIVATE, "{}", 1), Err(BugError::NotPublic));
    assert_eq!(
        parse_bug(r#"{"error":true,"code":102,"message":"no"}"#, "{}", 1),
        Err(BugError::NotPublic)
    );
    assert!(matches!(parse_bug("nope", "{}", 1), Err(BugError::Bad(_))));
}

#[test]
fn a_refused_host_builds_no_request_and_is_recorded() {
    let hosts = AllowedHosts::empty();
    let calls = RefCell::new(0);
    let get = |_: &str| {
        *calls.borrow_mut() += 1;
        Ok(String::new())
    };
    let r = read_bug("foo", "bsc#1234567", &hosts, &get);
    assert!(matches!(&r, BugRead::NotRead(w) if w.contains("hosts allow bugzilla.suse.com")));
    assert_eq!(*calls.borrow(), 0);
    let refused = hosts.refusals();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].0, BUGZILLA_HOST);
    assert_eq!(refused[0].2.package, "foo");
}

#[test]
fn an_allowed_host_is_read() {
    use crate::allowed_hosts::Origin;
    let hosts = AllowedHosts::empty();
    hosts.allow(BUGZILLA_HOST, Origin::Env, None).unwrap();
    let get = |url: &str| {
        Ok(if url.ends_with("/comment") {
            COMMENTS
        } else {
            BUG
        }
        .to_string())
    };
    match read_bug("foo", "bsc#1234567", &hosts, &get) {
        BugRead::Read(t) => assert_eq!(t.source, "bugzilla.suse.com bsc#1234567"),
        other => panic!("{other:?}"),
    }
    assert!(hosts.refusals().is_empty());
}

#[test]
fn a_private_answer_or_an_error_status_is_not_read() {
    use crate::allowed_hosts::Origin;
    let hosts = AllowedHosts::empty();
    hosts.allow(BUGZILLA_HOST, Origin::Env, None).unwrap();
    let r = read_bug("foo", "bsc#1", &hosts, &|_| Err(GetError::Status(401)));
    assert!(matches!(&r, BugRead::NotRead(w) if w.contains("not public")));
    let r = read_bug("foo", "bsc#1", &hosts, &|_| Ok(PRIVATE.to_string()));
    assert!(matches!(&r, BugRead::NotRead(w) if w.contains("not public")));
    let r = read_bug("foo", "bsc#1", &hosts, &|_| Err(GetError::Status(500)));
    assert!(matches!(&r, BugRead::NotRead(w) if w.contains("500")));
    let r = read_bug("foo", "jsc#SLE-1", &hosts, &|_| Ok(String::new()));
    assert!(matches!(r, BugRead::NotRead(_)));
}

#[test]
fn a_bug_file_is_the_one_comment() {
    let f = ReportFile {
        path: "bug.txt".into(),
        text: "it crashes".into(),
    };
    let t = BugText::from_file(&f);
    assert_eq!(t.source, "file bug.txt");
    assert_eq!(t.comments, ["it crashes"]);
}
