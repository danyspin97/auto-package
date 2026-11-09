use super::*;

fn start(pkg: &str, step: u32, attempt: u32, round: u32) -> String {
    format!(
        r#"{{"pkg":"{pkg}","event":"step_started","step":{step},"kind":"Build","attempt":{attempt},"round":{round},"detail":null}}"#
    )
}

fn step_end(pkg: &str, step: u32, outcome: &str) -> String {
    format!(
        r#"{{"pkg":"{pkg}","event":"step_finished","step":{step},"outcome":"{outcome}","elapsed":1000}}"#
    )
}

fn build_failed(pkg: &str, step: u32, classification: &str) -> String {
    format!(
        r#"{{"pkg":"{pkg}","event":"build_failed","step":{step},"classification":{classification},"log":"/w/build.log"}}"#
    )
}

fn finished(pkg: &str, outcome: &str) -> String {
    format!(r#"{{"event":"package_finished","pkg":"{pkg}","version":"1.0","outcome":{outcome}}}"#)
}

const BUILT: &str = r#""Built""#;
const FAILED: &str = r#"{"Failed":{"reason":"the build failed"}}"#;

/// The tries of the run's packages, read from the canned lines as a run's
/// `run.jsonl` gives them.
fn tries(lines: &[String]) -> Vec<PkgTries> {
    let (records, unreadable) = records_from_jsonl(&lines.join("\n"));
    assert_eq!(unreadable, 0);
    per_package(&records)
}

fn one(lines: &[String]) -> PkgTries {
    let mut all = tries(lines);
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

#[test]
fn a_package_built_on_its_first_try_is_one_try() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Ok"),
        finished("foo", BUILT),
    ]);
    assert_eq!(p.tries, 1);
    assert_eq!(p.outcome, TriesOutcome::Built);
    assert_eq!(p.reason, None);
    assert_eq!(p.attempts[0].result, "built");
    assert_eq!(
        distribution(&[p]),
        Distribution {
            one: 1,
            ..Default::default()
        }
    );
}

#[test]
fn a_package_built_after_one_failed_build_is_two_tries_and_the_failure_is_its_reason() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, r#""BuildFailed""#),
        start("foo", 2, 2, 1),
        step_end("foo", 2, "Ok"),
        finished("foo", BUILT),
    ]);
    assert_eq!(p.tries, 2);
    assert_eq!(p.outcome, TriesOutcome::Built);
    assert_eq!(p.attempts[0].reason.as_deref(), Some("BuildFailed"));
    assert_eq!(p.attempts[1].round, 1);
    assert_eq!(p.reason.as_deref(), Some("BuildFailed"));
    assert_eq!(p.shape.as_deref(), Some("BuildFailed"));
    let d = distribution(std::slice::from_ref(&p));
    assert_eq!((d.one, d.two, d.three_plus), (0, 1, 0));
}

#[test]
fn a_package_built_after_three_builds_is_flagged_with_its_last_failed_try() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, r#""PrepFailed""#),
        start("foo", 2, 2, 1),
        step_end("foo", 2, "Failed"),
        build_failed("foo", 2, r#""InstallFailed""#),
        start("foo", 3, 3, 2),
        step_end("foo", 3, "Ok"),
        finished("foo", BUILT),
    ]);
    assert_eq!(p.tries, 3);
    assert_eq!(distribution(std::slice::from_ref(&p)).three_plus, 1);
    assert_eq!(
        describe(&p),
        "foo: 3 tries, built; last failed try: InstallFailed"
    );
    assert_eq!(flagged(std::slice::from_ref(&p)).len(), 1);
}

#[test]
fn a_package_never_built_reports_its_tries_and_its_final_reason_and_shape() {
    let unresolvable = r#"{"UnresolvableDeps":{"packages":["bar-devel"]}}"#;
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, unresolvable),
        start("foo", 2, 2, 1),
        step_end("foo", 2, "Failed"),
        build_failed("foo", 2, unresolvable),
        finished("foo", r#"{"Failed":{"reason":"gave up after two builds"}}"#),
    ]);
    assert_eq!(p.tries, 2);
    assert_eq!(p.outcome, TriesOutcome::NotBuilt);
    assert_eq!(p.reason.as_deref(), Some("gave up after two builds"));
    assert_eq!(p.shape.as_deref(), Some("UnresolvableDeps: bar-devel"));
    assert_eq!(
        describe(&p),
        "foo: 2 tries, not built [UnresolvableDeps: bar-devel]: gave up after two builds"
    );
    assert_eq!(distribution(&[p]).not_built, 1);
}

#[test]
fn a_failed_build_without_a_classification_says_so() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        finished("foo", FAILED),
    ]);
    assert_eq!(
        p.attempts[0].reason.as_deref(),
        Some("failed, no classification recorded")
    );
    assert_eq!(p.reason.as_deref(), Some("the build failed"));
}

#[test]
fn an_outer_retry_is_a_further_try_even_though_its_attempt_counter_restarts() {
    // The frontier runs the package again: the second pipeline run counts its
    // attempts from 1 again and takes a new step id.
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, r#""BuildFailed""#),
        finished("foo", r#"{"Failed":{"reason":"first run"}}"#),
        start("foo", 5, 1, 0),
        step_end("foo", 5, "Ok"),
        finished("foo", BUILT),
    ]);
    assert_eq!(p.tries, 2);
    assert_eq!(p.outcome, TriesOutcome::Built);
    assert_eq!(
        p.attempts.iter().map(|t| t.step).collect::<Vec<_>>(),
        [1, 5]
    );
    assert_eq!(p.attempts[1].result, "built");
}

#[test]
fn a_package_finished_twice_with_identical_events_is_counted_once() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Ok"),
        finished("foo", BUILT),
        finished("foo", BUILT),
    ]);
    assert_eq!(p.tries, 1);
    assert_eq!(p.outcome, TriesOutcome::Built);
    assert_eq!(
        distribution(&[p]),
        Distribution {
            one: 1,
            ..Default::default()
        }
    );
}

#[test]
fn an_already_built_package_with_no_build_is_not_counted() {
    let lines = [
        finished("foo", r#""AlreadyBuilt""#),
        finished("bar", r#""AlreadyBuilt""#),
    ];
    let all = tries(&lines);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|p| p.tries == 0));
    assert!(all.iter().all(|p| p.outcome == TriesOutcome::AlreadyBuilt));
    assert_eq!(distribution(&all), Distribution::default());
    assert!(flagged(&all).is_empty());
    assert!(!ran_a_build(&all));
    assert_eq!(summary_block(&all), "");
}

#[test]
fn a_build_that_never_finished_is_no_result_with_the_tries_it_made() {
    let p = one(&[start("foo", 1, 1, 0)]);
    assert_eq!(p.tries, 1);
    assert_eq!(p.outcome, TriesOutcome::NoResult);
    assert_eq!(p.attempts[0].result, "not finished");
    assert!(
        p.reason
            .as_deref()
            .unwrap()
            .starts_with("no package_finished")
    );
    assert_eq!(distribution(&[p]).not_built, 1);
}

#[test]
fn a_disk_full_failure_is_its_own_shape() {
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        finished(
            "foo",
            r#"{"Failed":{"reason":"no space left","disk_full":true}}"#,
        ),
    ]);
    assert_eq!(p.shape.as_deref(), Some("DiskFull"));
    assert_eq!(p.reason.as_deref(), Some("disk full: no space left"));
}

#[test]
fn the_log_extract_of_a_failed_build_is_part_of_its_shape() {
    let extract = r#"{"pkg":"foo","event":"extract_result","step":2,"extract":{"genre":"Compile","phase":"%build","located_by":"marker","excerpt":"","error_lines":[],"elided_count":0,"source_candidates":[],"has_signal":true}, "artifact":"/w/extract.txt"}"#;
    let p = one(&[
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, r#""BuildFailed""#),
        extract.to_string(),
        finished("foo", FAILED),
    ]);
    assert_eq!(p.shape.as_deref(), Some("BuildFailed (Compile/%build)"));
}

#[test]
fn lines_that_name_a_step_but_do_not_read_are_counted_and_others_ignored() {
    let text = [
        r#"{"pkg":"foo","event":"step_started","step":1,"kind":"Build"}"#.to_string(),
        r#"{"pkg":"foo","event":"step_started","step":"x","kind":"Build"}"#.to_string(),
        r#"{"pkg":"foo","event":"warning","step":1,"message":"hi"}"#.to_string(),
        "not json \"event\":\"package_finished\"".to_string(),
    ]
    .join("\n");
    let (records, unreadable) = records_from_jsonl(&text);
    assert_eq!(records.len(), 1);
    assert_eq!(unreadable, 2);
}

#[test]
fn the_summary_lists_at_most_twenty_five_lines_of_packages() {
    let lines: Vec<String> = (0..30)
        .flat_map(|i| {
            let pkg = format!("pkg{i:02}");
            [
                start(&pkg, 1, 1, 0),
                step_end(&pkg, 1, "Failed"),
                finished(&pkg, FAILED),
            ]
        })
        .collect();
    let all = tries(&lines);
    let block = summary_block(&all);
    let mut it = block.lines();
    assert_eq!(it.next(), Some(""));
    assert_eq!(
        it.next(),
        Some("Tries to build: 1 try: 0, 2 tries: 0, 3+ tries: 0, not built: 30")
    );
    let listed: Vec<&str> = it.collect();
    assert_eq!(listed.len(), SUMMARY_LINES);
    assert_eq!(listed.last(), Some(&"  ... and 6 more"));
}

#[test]
fn the_summary_block_is_empty_when_no_package_ran_a_build() {
    assert_eq!(summary_block(&[]), "");
}
