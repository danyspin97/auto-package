use crate::unconfirmed_offer::*;
use ap_build::pipeline::{Candidate, CandidateSource};

const TTY: Terminal = Terminal {
    stdin: true,
    stderr: true,
    tui: false,
};

fn good() -> Candidate {
    Candidate {
        stars: Some(222),
        releases: Some(30),
        archived: Some(false),
        fork: Some(false),
        ..Candidate::new(
            "https://github.com/k4ditano/pleamar",
            CandidateSource::Agent,
        )
    }
}

fn other() -> Candidate {
    Candidate {
        stars: Some(23),
        releases: Some(1),
        archived: Some(false),
        fork: Some(false),
        ..Candidate::new(
            "https://github.com/k4ditano/pleamar-wm",
            CandidateSource::Search,
        )
    }
}

fn offered(list: &[Candidate], term: Terminal) -> Option<String> {
    offerable("pleamar", list, term).map(|c| c.url.clone())
}

#[test]
fn the_clear_candidate_is_offered_with_its_facts() {
    let list = [good(), other()];
    assert_eq!(
        offered(&list, TTY).as_deref(),
        Some("https://github.com/k4ditano/pleamar")
    );
    assert_eq!(
        question(&list[0]),
        "use github.com/k4ditano/pleamar (222 stars, 30 releases)?"
    );
}

#[test]
fn the_sole_exact_hit_is_offered_without_an_agent_pick() {
    let mut c = good();
    c.source = CandidateSource::Search;
    assert!(offered(&[c, other()], TTY).is_some());
}

#[test]
fn each_failing_condition_withholds_the_offer() {
    let mut twin = good();
    twin.url = "https://github.com/ramiroAGI/pleamar".into();
    twin.source = CandidateSource::Search;
    assert_eq!(offered(&[good(), twin], TTY), None, "two exact hits");

    let mut c = good();
    c.archived = Some(true);
    assert_eq!(offered(&[c], TTY), None, "archived");
    let mut c = good();
    c.fork = Some(true);
    assert_eq!(offered(&[c], TTY), None, "fork");
    let mut c = good();
    c.releases = Some(0);
    assert_eq!(offered(&[c], TTY), None, "no releases");
    for unknown in 0..3 {
        let mut c = good();
        match unknown {
            0 => c.releases = None,
            1 => c.archived = None,
            _ => c.fork = None,
        }
        assert_eq!(offered(&[c], TTY), None, "unknown metadata {unknown}");
    }

    // The agent picked another repository than the only exact-name hit.
    let mut hit = good();
    hit.source = CandidateSource::Search;
    let mut pick = other();
    pick.source = CandidateSource::Agent;
    assert_eq!(offered(&[hit, pick], TTY), None, "agent disagrees");

    assert_eq!(offered(&[other()], TTY), None, "no exact name");
    for term in [
        Terminal {
            stdin: false,
            ..TTY
        },
        Terminal {
            stderr: false,
            ..TTY
        },
        Terminal { tui: true, ..TTY },
    ] {
        assert_eq!(offered(&[good()], term), None, "not interactive");
    }
}

#[test]
fn only_yes_in_any_case_confirms() {
    let said = |a: Option<&'static str>| {
        let seen = std::cell::RefCell::new(String::new());
        let r = confirm(&good(), &|p| {
            *seen.borrow_mut() = p.to_string();
            a.map(str::to_string)
        });
        assert_eq!(
            seen.into_inner(),
            "use github.com/k4ditano/pleamar (222 stars, 30 releases)? [y/N] "
        );
        r
    };
    assert!(said(Some("y")));
    assert!(said(Some("YES")));
    assert!(said(Some(" Yes ")));
    assert!(!said(Some("")));
    assert!(!said(Some("n")));
    assert!(!said(Some("yep")));
    assert!(!said(None));
}

#[test]
fn a_confirmed_repository_is_written_to_sources_toml_and_found_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut before = ap_build::sources::SourceMap::default();
    before.insert("other", "wheel");
    before.save(dir.path()).unwrap();

    remember(dir.path(), "pleamar", "https://github.com/k4ditano/pleamar").unwrap();
    let text = std::fs::read_to_string(dir.path().join("sources.toml")).unwrap();
    assert!(
        text.contains("pleamar = \"https://github.com/k4ditano/pleamar\""),
        "{text}"
    );

    let map = ap_build::sources::SourceMap::load(dir.path()).unwrap();
    assert_eq!(
        remembered(&map, "Pleamar").as_deref(),
        Some("https://github.com/k4ditano/pleamar")
    );
    // The earlier entry is kept, and a `wheel` entry is no repository.
    assert_eq!(map.get("other"), Some("wheel"));
    assert_eq!(remembered(&map, "other"), None);
    assert_eq!(remembered(&map, "absent"), None);
}
