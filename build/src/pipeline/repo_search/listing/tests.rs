use super::*;
use std::sync::Mutex;

fn found(hits: &[&str], pick: Option<&str>) -> RepoCandidates {
    RepoCandidates {
        unvouched_search_hits: hits.iter().map(|s| s.to_string()).collect(),
        agent_pick: pick.map(str::to_string),
        ..Default::default()
    }
}

fn urls(list: &[Candidate]) -> Vec<&str> {
    list.iter().map(|c| c.url.as_str()).collect()
}

#[test]
fn exact_names_come_before_close_ones_and_the_agents_pick_last() {
    let f = found(
        &[
            "https://github.com/k4ditano/pleamar-wm",
            "https://github.com/k4ditano/pleamar",
            "https://github.com/ramiroAGI/pleamar",
        ],
        Some("https://gitlab.com/x/pleamar-ng"),
    );
    let list = candidate_list(&f, "pleamar");
    assert_eq!(
        urls(&list),
        [
            "https://github.com/k4ditano/pleamar",
            "https://github.com/ramiroAGI/pleamar",
            "https://github.com/k4ditano/pleamar-wm",
            "https://gitlab.com/x/pleamar-ng",
        ]
    );
    assert_eq!(list[3].source, CandidateSource::Agent);
    assert_eq!(list[0].source, CandidateSource::Search);
}

#[test]
fn a_hit_the_agent_also_picked_is_listed_once_as_the_agents() {
    let f = found(
        &["https://github.com/k4ditano/pleamar"],
        Some("https://GitHub.com/K4ditano/Pleamar.git/"),
    );
    let list = candidate_list(&f, "pleamar");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].source, CandidateSource::Agent);
    assert_eq!(list[0].url, "https://github.com/k4ditano/pleamar");
}

#[test]
fn names_compare_after_normalisation() {
    let list = candidate_list(&found(&["https://github.com/o/Foo_Bar"], None), "foo-bar");
    assert!(list[0].is_exact("foo.bar"));
    assert!(!list[0].is_exact("foo"));
    assert!(
        Candidate::new("https://github.com/o/foo.git", CandidateSource::Search).is_exact("foo")
    );
}

#[test]
fn at_most_five_and_none_without_hits() {
    let hits: Vec<String> = (0..9)
        .map(|i| format!("https://github.com/o{i}/x"))
        .collect();
    let refs: Vec<&str> = hits.iter().map(String::as_str).collect();
    assert_eq!(candidate_list(&found(&refs, None), "x").len(), 5);
    assert!(candidate_list(&found(&[], None), "x").is_empty());
}

fn fixture(calls: &Mutex<Vec<String>>, url: &str) -> Option<serde_json::Value> {
    calls.lock().unwrap().push(url.to_string());
    match url {
        "https://api.github.com/repos/k4ditano/pleamar" => {
            Some(serde_json::json!({"stargazers_count": 222, "archived": false, "fork": false}))
        }
        "https://api.github.com/repos/k4ditano/pleamar/releases?per_page=100" => {
            Some(serde_json::json!((0..30).map(|_| {}).collect::<Vec<_>>()))
        }
        "https://api.github.com/repos/ramiroAGI/pleamar" => {
            Some(serde_json::json!({"stargazers_count": 0, "archived": true, "fork": true}))
        }
        _ => None,
    }
}

#[test]
fn metadata_is_read_from_the_answers_and_a_failure_leaves_none() {
    let calls = Mutex::new(Vec::new());
    let mut list = candidate_list(
        &found(
            &[
                "https://github.com/k4ditano/pleamar",
                "https://github.com/ramiroAGI/pleamar",
                "https://github.com/gone/pleamar",
            ],
            None,
        ),
        "pleamar",
    );
    fill_metadata(&mut list, &|u| fixture(&calls, u));
    assert_eq!(list[0].stars, Some(222));
    assert_eq!(list[0].releases, Some(30));
    assert_eq!(list[0].archived, Some(false));
    assert_eq!(list[1].archived, Some(true));
    assert_eq!(list[1].fork, Some(true));
    // The releases request failed: unknown, not zero.
    assert_eq!(list[1].releases, None);
    assert_eq!(
        list[2],
        Candidate::new("https://github.com/gone/pleamar", CandidateSource::Search)
    );
    // Two requests for the first, two for the second, one for the third.
    assert_eq!(calls.lock().unwrap().len(), 5);
}

#[test]
fn no_request_is_made_without_a_github_candidate() {
    let calls = Mutex::new(Vec::new());
    let mut list = vec![
        Candidate::new("https://gitlab.com/o/x", CandidateSource::Search),
        Candidate::new("https://codeberg.org/o/x", CandidateSource::Agent),
    ];
    fill_metadata(&mut list, &|u| fixture(&calls, u));
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(list[0].stars, None);
}
