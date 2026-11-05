use std::cell::RefCell;
use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;
use crate::patch_provenance::{OriginEvidence, OriginKind, PatchOrigin};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const REPO: &str = "https://github.com/o/r";

/// Answers from a table; counts the calls it gets.
#[derive(Default)]
pub(crate) struct FakeForge {
    pub json: HashMap<String, Result<Value, String>>,
    pub tags: Vec<String>,
    pub patches: HashMap<String, String>,
    pub calls: RefCell<Vec<String>>,
}

impl ForgeNet for FakeForge {
    fn api_json(&self, url: &str) -> Result<Value, String> {
        self.calls.borrow_mut().push(url.to_string());
        match self.json.get(url) {
            Some(answer) => answer.clone(),
            None => Err(format!("404 {url}")),
        }
    }
    fn patch_text(&self, url: &str) -> Result<String, String> {
        self.calls.borrow_mut().push(url.to_string());
        self.patches
            .get(url)
            .cloned()
            .ok_or_else(|| format!("no patch {url}"))
    }
    fn release_tags(&self, _repo: &str) -> Result<Vec<String>, String> {
        Ok(self.tags.clone())
    }
}

fn origin(url: &str, kind: OriginKind, project: &str, number: u64) -> PatchOrigin {
    PatchOrigin {
        url: url.into(),
        kind,
        project: project.into(),
        number: Some(number),
        commit: None,
        evidence: OriginEvidence::PatchHeader,
    }
}

fn gh_pull() -> PatchOrigin {
    origin("https://github.com/o/r/pull/106", OriginKind::Pr, REPO, 106)
}

#[test]
fn state_urls_per_forge() {
    assert_eq!(
        state_url(&gh_pull()).as_deref(),
        Some("https://api.github.com/repos/o/r/pulls/106")
    );
    let gitea = origin(
        "https://codeberg.org/o/r/pulls/3",
        OriginKind::Pr,
        "https://codeberg.org/o/r",
        3,
    );
    assert_eq!(
        state_url(&gitea).as_deref(),
        Some("https://codeberg.org/api/v1/repos/o/r/pulls/3")
    );
    let mr = origin(
        "https://gitlab.com/g/sub/p/-/merge_requests/7",
        OriginKind::Mr,
        "https://gitlab.com/g/sub/p",
        7,
    );
    assert_eq!(
        state_url(&mr).as_deref(),
        Some("https://gitlab.com/api/v4/projects/g%2Fsub%2Fp/merge_requests/7")
    );
    let commit = origin(
        &format!("https://github.com/o/r/commit/{SHA}"),
        OriginKind::Commit,
        REPO,
        0,
    );
    let commit = PatchOrigin {
        number: None,
        ..commit
    };
    assert_eq!(state_url(&commit), None);
}

#[test]
fn a_github_pull_request_is_open_merged_or_closed() {
    let o = gh_pull();
    let open = json!({"state": "open", "merged": false, "merge_commit_sha": null,
        "head": {"sha": SHA}, "title": "[PATCH] Split the crt0"});
    let got = parse_state(&o, &open).unwrap();
    assert_eq!(got.state, PrState::Open);
    assert_eq!(got.merge_sha, None);
    assert_eq!(got.head_sha, SHA);
    assert_eq!(got.title, "Split the crt0");
    let merged = json!({"state": "closed", "merged": true, "merge_commit_sha": SHA2,
        "head": {"sha": SHA}, "title": "x"});
    let got = parse_state(&o, &merged).unwrap();
    assert_eq!(got.state, PrState::Merged);
    assert_eq!(got.merge_sha.as_deref(), Some(SHA2));
    let closed = json!({"state": "closed", "merged": false, "head": {"sha": SHA}});
    assert_eq!(parse_state(&o, &closed).unwrap().state, PrState::Closed);
}

#[test]
fn bad_ids_and_unknown_states_are_refused() {
    let o = gh_pull();
    let bad_head = json!({"state": "open", "merged": false, "head": {"sha": "../../x"}});
    assert!(parse_state(&o, &bad_head).is_err());
    let weird = json!({"state": "pending", "merged": false, "head": {"sha": SHA}});
    assert!(parse_state(&o, &weird).is_err());
    let merged_no_sha =
        json!({"state": "closed", "merged": true, "merge_commit_sha": "zz", "head": {"sha": SHA}});
    assert!(parse_state(&o, &merged_no_sha).is_err());
}

#[test]
fn a_title_is_sanitised_and_cut_at_120() {
    let o = gh_pull();
    let title = format!("[PATCH] a\u{202E}b {}", "w".repeat(200));
    let j = json!({"state": "open", "merged": false, "head": {"sha": SHA}, "title": title});
    let got = parse_state(&o, &j).unwrap();
    assert!(!got.title.contains('\u{202E}'));
    assert!(got.title.chars().count() <= 120);
    assert!(got.title.starts_with("a b"));
}

#[test]
fn a_gitlab_merge_request_reads_its_states_and_squash_sha() {
    let mr = origin(
        "https://gitlab.com/g/p/-/merge_requests/7",
        OriginKind::Mr,
        "https://gitlab.com/g/p",
        7,
    );
    let merged = json!({"state": "merged", "squash_commit_sha": SHA2, "sha": SHA, "title": "t"});
    let got = parse_state(&mr, &merged).unwrap();
    assert_eq!(got.state, PrState::Merged);
    assert_eq!(got.merge_sha.as_deref(), Some(SHA2));
    let opened = json!({"state": "opened", "sha": SHA});
    assert_eq!(parse_state(&mr, &opened).unwrap().state, PrState::Open);
}

#[test]
fn containment_asks_the_packaged_tag_then_the_newer_ones() {
    let o = gh_pull();
    let mut fake = FakeForge {
        tags: vec!["v1.3".into(), "v1.2".into(), "v1.1".into()],
        ..Default::default()
    };
    fake.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.2...{SHA}"),
        Ok(json!({"status": "diverged"})),
    );
    fake.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.3...{SHA}"),
        Ok(json!({"status": "behind"})),
    );
    assert_eq!(
        containment_of(&fake, &o, REPO, "1.2", SHA),
        Containment::InNewer {
            tag: "v1.3".into(),
            version: "1.3".into()
        }
    );
    fake.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.2...{SHA}"),
        Ok(json!({"status": "identical"})),
    );
    assert_eq!(
        containment_of(&fake, &o, REPO, "1.2", SHA),
        Containment::InPackaged { tag: "v1.2".into() }
    );
}

#[test]
fn containment_says_not_released_and_not_checked() {
    let o = gh_pull();
    let mut fake = FakeForge {
        tags: vec!["v1.2".into()],
        ..Default::default()
    };
    fake.json.insert(
        format!("https://api.github.com/repos/o/r/compare/v1.2...{SHA}"),
        Ok(json!({"status": "diverged"})),
    );
    assert_eq!(
        containment_of(&fake, &o, REPO, "1.1", SHA),
        Containment::NotReleased
    );
    fake.calls.borrow_mut().clear();
    let other = origin(
        "https://github.com/x/y/pull/1",
        OriginKind::Pr,
        "https://github.com/x/y",
        1,
    );
    let got = containment_of(&fake, &other, REPO, "1.1", SHA);
    assert!(matches!(got, Containment::NotChecked(_)), "{got:?}");
    assert!(
        fake.calls.borrow().is_empty(),
        "no call for another project"
    );
    let gitea = origin(
        "https://codeberg.org/o/r/pulls/3",
        OriginKind::Pr,
        "https://codeberg.org/o/r",
        3,
    );
    let got = containment_of(&fake, &gitea, "https://codeberg.org/o/r", "1.1", SHA);
    assert!(matches!(got, Containment::NotChecked(_)), "{got:?}");
}

#[test]
fn a_gitlab_compare_with_no_commits_is_contained() {
    let mr = origin(
        "https://gitlab.com/g/p/-/merge_requests/7",
        OriginKind::Mr,
        "https://gitlab.com/g/p",
        7,
    );
    let mut fake = FakeForge {
        tags: vec!["v2.0".into(), "v1.0".into()],
        ..Default::default()
    };
    fake.json.insert(
        format!("https://gitlab.com/api/v4/projects/g%2Fp/repository/compare?from=v1.0&to={SHA}"),
        Ok(json!({"commits": [{}]})),
    );
    fake.json.insert(
        format!("https://gitlab.com/api/v4/projects/g%2Fp/repository/compare?from=v2.0&to={SHA}"),
        Ok(json!({"commits": []})),
    );
    assert_eq!(
        containment_of(&fake, &mr, "https://gitlab.com/g/p", "1.0", SHA),
        Containment::InNewer {
            tag: "v2.0".into(),
            version: "2.0".into()
        }
    );
}

#[test]
fn a_failing_tag_list_is_not_checked() {
    struct NoTags;
    impl ForgeNet for NoTags {
        fn api_json(&self, _: &str) -> Result<Value, String> {
            Err("unused".into())
        }
        fn patch_text(&self, _: &str) -> Result<String, String> {
            Err("unused".into())
        }
        fn release_tags(&self, _: &str) -> Result<Vec<String>, String> {
            Err("rate limit".into())
        }
    }
    assert_eq!(
        containment_of(&NoTags, &gh_pull(), REPO, "1.0", SHA),
        Containment::NotChecked("rate limit".into())
    );
}

#[test]
fn diff_url_is_the_page_with_patch() {
    assert_eq!(
        diff_url(&gh_pull()),
        "https://github.com/o/r/pull/106.patch"
    );
}

#[test]
fn a_release_tag_with_dots_or_slashes_is_not_put_in_a_path() {
    use super::status::plain_tag;
    assert!(plain_tag("v1.2.3"));
    assert!(plain_tag("release/2.0+rc1"));
    assert!(!plain_tag("../../x"));
    assert!(!plain_tag("-X"));
    assert!(!plain_tag("a b"));
}

#[test]
fn a_search_cut_at_four_newer_releases_is_not_reported_as_not_released() {
    let o = gh_pull();
    let mut fake = FakeForge {
        tags: vec![
            "v1.6".into(),
            "v1.5".into(),
            "v1.4".into(),
            "v1.3".into(),
            "v1.2".into(),
            "v1.1".into(),
        ],
        ..Default::default()
    };
    for t in ["v1.2", "v1.3", "v1.4", "v1.5", "v1.6"] {
        fake.json.insert(
            format!("https://api.github.com/repos/o/r/compare/{t}...{SHA}"),
            Ok(json!({"status": "diverged"})),
        );
    }
    let got = containment_of(&fake, &o, REPO, "1.1", SHA);
    assert!(matches!(got, Containment::NotChecked(_)), "{got:?}");
}
