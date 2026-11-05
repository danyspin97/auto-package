use super::*;
use std::cell::RefCell;
use std::time::Duration;

fn answer(code: i32, stderr: &str) -> (i32, &str) {
    (code, stderr)
}

/// Runs `answers` in order (the last one repeats) as the tries of a query, and
/// returns the last answer, the number of tries and each pause taken.
fn run_with(answers: Vec<(i32, &str)>) -> (Result<proc::Captured, ()>, usize, Vec<Duration>) {
    let tries = RefCell::new(0usize);
    let pauses = RefCell::new(Vec::new());
    let out = retry_gateway_failures(
        || {
            let mut n = tries.borrow_mut();
            let (code, stderr) = answers[(*n).min(answers.len() - 1)];
            *n += 1;
            Ok(proc::Captured {
                code,
                stdout: String::new(),
                stderr: stderr.to_string(),
            })
        },
        |d| pauses.borrow_mut().push(d),
    );
    (out, *tries.borrow(), pauses.into_inner())
}

#[test]
fn a_gateway_failure_is_repeated_and_a_later_success_is_the_answer() {
    let (out, tries, pauses) = run_with(vec![
        answer(
            1,
            "Server returned an error: HTTP Error 503: Service Unavailable",
        ),
        answer(0, "ok"),
    ]);
    assert_eq!(out.unwrap().stdout, "");
    assert_eq!(tries, 2);
    assert_eq!(pauses, [Duration::from_secs(2)]);
}

#[test]
fn a_gateway_failure_that_persists_is_tried_three_times_then_given_up() {
    let (out, tries, pauses) = run_with(vec![answer(1, "HTTP Error 502: Bad Gateway")]);
    assert_eq!(out.unwrap().code, 1);
    assert_eq!(tries, QUERY_ATTEMPTS as usize);
    assert_eq!(pauses, [Duration::from_secs(2), Duration::from_secs(4)]);
}

#[test]
fn a_504_is_a_gateway_failure_too() {
    let (_, tries, _) = run_with(vec![answer(1, "HTTP Error 504: Gateway Time-out")]);
    assert_eq!(tries, 3);
}

#[test]
fn a_not_found_or_a_forbidden_answer_is_final_at_once() {
    for stderr in [
        "HTTP Error 404: Not Found",
        "HTTP Error 403: Forbidden",
        "boom",
    ] {
        let (out, tries, pauses) = run_with(vec![answer(1, stderr)]);
        assert_eq!(out.unwrap().code, 1, "{stderr}");
        assert_eq!(tries, 1, "{stderr}");
        assert!(pauses.is_empty(), "{stderr}");
    }
}

#[test]
fn a_launch_failure_is_returned_as_it_is() {
    let tries = RefCell::new(0usize);
    let out: Result<proc::Captured, &str> = retry_gateway_failures(
        || {
            *tries.borrow_mut() += 1;
            Err("no osc")
        },
        |_| panic!("no pause for a launch failure"),
    );
    assert!(matches!(out, Err("no osc")));
    assert_eq!(*tries.borrow(), 1);
}

#[test]
fn a_success_is_not_repeated() {
    let (out, tries, _) = run_with(vec![answer(0, "")]);
    assert!(out.unwrap().success());
    assert_eq!(tries, 1);
}

#[test]
fn only_a_plain_api_get_is_retried_and_a_write_is_not() {
    assert!(api_is_retried(None, None));
    assert!(!api_is_retried(Some("POST"), None));
    assert!(!api_is_retried(Some("DELETE"), None));
    assert!(!api_is_retried(None, Some("<person/>")));
}

/// The osc files that may call the retrying reader (`capture_query`). Each one
/// reads from OBS: a query, a listing, a meta read. A file that writes to OBS
/// must not be added, and a new caller fails this test until it is reviewed.
const RETRYING_READERS: &[&str] = &[
    "branch/api.rs",
    "branch/project_meta.rs",
    "project_packages.rs",
    "query.rs",
    "repo_paths.rs",
    "status.rs",
];

fn osc_sources(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            osc_sources(&path, base, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && !path.to_string_lossy().ends_with("_tests.rs")
        {
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("capture_query(") || text.contains("retry_gateway_failures(") {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
}

/// Limit of this check: it looks at files, not functions. A write added to one
/// of the listed reader files would not be seen here; the write helpers
/// (`write_meta`, `ci`, `rm`) use `proc::capture_with_timeout` directly today.
#[test]
fn only_the_listed_read_paths_reach_the_gateway_retry() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/osc");
    let mut found = Vec::new();
    osc_sources(&base, &base, &mut found);
    found.sort();
    let mut listed: Vec<String> = RETRYING_READERS.iter().map(|s| s.to_string()).collect();
    listed.sort();
    // query.rs defines the retrying reader, so it is one of the listed files.
    assert_eq!(found, listed);
}
