use super::*;
use crate::allowed_hosts::AllowedHosts;
use crate::allowed_hosts::Origin;

const OSV: &str = include_str!("../testdata/bugfix/osv_CVE-2025-1234.json");
const MITRE: &str = include_str!("../testdata/bugfix/cve_CVE-2025-1234.json");

#[test]
fn osv_gives_the_fixing_commit_first_then_the_references() {
    let t = parse_osv(OSV).unwrap();
    assert_eq!(t.summary, "Heap overflow in foo parse_token");
    assert_eq!(
        t.urls,
        [
            "https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567.patch",
            "https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567",
            "https://example.org/advisory",
        ]
    );
    assert!(t.comments[0].contains("without checking"));
}

#[test]
fn mitre_gives_the_description_and_every_reference_once() {
    let t = parse_mitre(MITRE).unwrap();
    assert_eq!(
        t.summary,
        "A heap overflow in parse_token() of foo before 1.2.4."
    );
    assert_eq!(t.urls.len(), 3);
    assert!(t.comments[0].contains("An attacker"));
}

#[test]
fn a_malformed_record_is_an_error() {
    for bad in ["", "[]", "{\"id\":", "{}"] {
        assert!(parse_osv(bad).is_err(), "{bad}");
        assert!(parse_mitre(bad).is_err(), "{bad}");
    }
}

#[test]
fn no_source_is_read_unless_the_user_allowed_it() {
    let hosts = AllowedHosts::empty();
    let get = |_: &str| -> Result<String, GetError> { panic!("no request") };
    let r = read_cve("foo", "CVE-2025-1234", &hosts, &get);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|r| matches!(r, CveRead::NotRead { .. })));
    let refused: Vec<String> = hosts.refusals().into_iter().map(|r| r.0).collect();
    assert_eq!(refused, [OSV_HOST, MITRE_HOST]);
    // Never the vendor's page.
    assert!(!refused.iter().any(|h| h.contains("suse.com")));
}

#[test]
fn an_allowed_source_is_read_and_the_other_is_not() {
    let hosts = AllowedHosts::empty();
    hosts.allow(OSV_HOST, Origin::Env, None).unwrap();
    let get = |url: &str| {
        assert_eq!(url, "https://api.osv.dev/v1/vulns/CVE-2025-1234");
        Ok(OSV.to_string())
    };
    let r = read_cve("foo", "CVE-2025-1234", &hosts, &get);
    match &r[0] {
        CveRead::Read(t) => assert_eq!(t.source, "api.osv.dev CVE-2025-1234"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(&r[1], CveRead::NotRead { host, .. } if host == MITRE_HOST));
}

#[test]
fn an_unknown_cve_or_a_failure_is_not_read() {
    let hosts = AllowedHosts::empty();
    hosts.allow(OSV_HOST, Origin::Env, None).unwrap();
    hosts.allow(MITRE_HOST, Origin::Env, None).unwrap();
    let r = read_cve("foo", "CVE-2025-1234", &hosts, &|_| {
        Err(GetError::Status(404))
    });
    assert!(
        r.iter()
            .all(|r| matches!(r, CveRead::NotRead { why, .. } if why.contains("not known")))
    );
    let r = read_cve("foo", "bsc#1234567", &hosts, &|_| Ok(String::new()));
    assert!(matches!(&r[..], [CveRead::NotRead { .. }]));
}
