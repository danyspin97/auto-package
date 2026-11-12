use super::*;
use unicode_width::UnicodeWidthStr;

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
    assert_eq!(summary_block(&all, 100), "");
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
    let block = summary_block(&all, 100);
    let mut it = block.lines();
    assert_eq!(it.next(), Some(""));
    assert_eq!(it.next(), Some("Tries to build (30):"));
    assert_eq!(it.next(), Some("  not built: 30"));
    let listed: Vec<&str> = it.collect();
    assert_eq!(listed.len(), SUMMARY_LINES);
    assert_eq!(listed.last(), Some(&"    ... and 6 more"));
}

#[test]
fn the_summary_block_is_empty_when_no_package_ran_a_build() {
    assert_eq!(summary_block(&[], 100), "");
}

#[test]
fn explain_prints_the_tries_and_the_reason_of_each_failed_try() {
    let t = tempfile::tempdir().unwrap();
    let run = t.path().join("runs").join("20261027-000000-aaaa-1-0");
    std::fs::create_dir_all(&run).unwrap();
    let lines = [
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Failed"),
        build_failed("foo", 1, r#""BuildFailed""#),
        start("foo", 2, 2, 1),
        step_end("foo", 2, "Ok"),
        finished("foo", BUILT),
    ];
    std::fs::write(run.join("run.jsonl"), lines.join("\n") + "\n").unwrap();
    let ex = explain(&Query {
        project_dir: t.path(),
        package: "foo",
        run: None,
        package_notes: String::new(),
    })
    .unwrap();
    let text = plain(&render(&ex, 100));
    assert!(text.contains("Tries (2)"), "{text}");
    assert!(text.contains("2 tries, built"), "{text}");
    assert!(text.contains("try 1: failed, BuildFailed"), "{text}");
    assert!(text.contains("try 2: built"), "{text}");
}

#[test]
fn the_whole_run_counts_every_package_and_lists_the_ones_to_look_at() {
    let t = tempfile::tempdir().unwrap();
    let run = t.path().join("runs").join("20261027-000000-aaaa-1-0");
    std::fs::create_dir_all(&run).unwrap();
    let lines = [
        start("foo", 1, 1, 0),
        step_end("foo", 1, "Ok"),
        finished("foo", BUILT),
        start("bar", 1, 1, 0),
        step_end("bar", 1, "Failed"),
        finished("bar", FAILED),
    ];
    std::fs::write(run.join("run.jsonl"), lines.join("\n") + "\n").unwrap();
    let rt = run_tries(t.path(), None).unwrap();
    assert_eq!(rt.run, "20261027-000000-aaaa-1-0");
    assert_eq!(rt.packages.len(), 2);
    let text = plain(&render_run(&rt, 100));
    assert!(
        text.contains("1 try: 1, 2 tries: 0, 3+ tries: 0, not built: 1"),
        "{text}"
    );
    assert!(
        text.contains("bar: 1 try, not built: the build failed"),
        "{text}"
    );
    assert!(!text.contains("foo: 1 try"), "{text}");
    assert!(run_tries(t.path(), Some("nope")).is_err());
}

#[test]
fn the_project_of_a_whole_run_view_is_the_work_dir_when_it_holds_runs() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("runs")).unwrap();
    assert_eq!(resolve_run_dir(t.path(), None, "x86_64").unwrap(), t.path());
}

/// The lines of a package built on try `tries`, its earlier tries failing
/// with the classification `failure`.
fn built_on_try(pkg: &str, tries: u32, failure: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for k in 1..tries {
        lines.push(start(pkg, k, k, k - 1));
        lines.push(step_end(pkg, k, "Failed"));
        lines.push(build_failed(pkg, k, &format!("\"{failure}\"")));
    }
    lines.push(start(pkg, tries, tries, tries - 1));
    lines.push(step_end(pkg, tries, "Ok"));
    lines.push(finished(pkg, BUILT));
    lines
}

/// The lines of a package whose one build failed with `reason`.
fn never_built(pkg: &str, reason: &str) -> Vec<String> {
    vec![
        start(pkg, 1, 1, 0),
        step_end(pkg, 1, "Failed"),
        build_failed(pkg, 1, r#""BuildFailed""#),
        finished(pkg, &format!(r#"{{"Failed":{{"reason":"{reason}"}}}}"#)),
    ]
}

#[test]
fn the_summary_block_with_one_bucket_prints_only_that_bucket() {
    let all = tries(&built_on_try("foo", 1, "BuildFailed"));
    assert_eq!(
        summary_block(&all, 100),
        "\nTries to build (1):\n  1 try: 1\n"
    );
}

#[test]
fn the_summary_block_lists_only_the_buckets_that_are_not_empty() {
    let mut lines = built_on_try("a", 1, "BuildFailed");
    lines.extend(built_on_try("b", 2, "BuildFailed"));
    lines.extend(built_on_try("c", 3, "InstallFailed"));
    lines.extend(never_built("d", "x"));
    let all = tries(&lines);
    assert_eq!(
        summary_block(&all, 100),
        "\nTries to build (4):\n  1 try: 1, 2 tries: 1, 3+ tries: 1, not built: 1\n\
         \x20   c  3 tries, built · last failed try: install failed\n\
         \x20   d  1 try, not built [build failed] · x\n"
    );
}

#[test]
fn the_users_sample_reads_as_planned() {
    let all = tries(&built_on_try("jay-compositor", 4, "InstallFailed"));
    assert_eq!(
        summary_block(&all, 100),
        "\nTries to build (1):\n  3+ tries: 1\n    jay-compositor  4 tries, built · last failed try: install failed\n"
    );
}

#[test]
fn a_no_result_package_says_it_has_no_result() {
    let lines = vec![start("pkg", 1, 1, 0), step_end("pkg", 1, "Ok")];
    let block = summary_block(&tries(&lines), 100);
    assert!(
        block.contains("    pkg  1 try, no result · no package_finished: the run was"),
        "{block}"
    );
}

#[test]
fn wide_names_align_by_display_width() {
    let mut lines = built_on_try("日本語", 3, "BuildFailed");
    lines.extend(built_on_try("ab", 3, "BuildFailed"));
    let block = summary_block(&tries(&lines), 100);
    let rows: Vec<&str> = block.lines().skip(3).collect();
    assert_eq!(
        rows,
        vec![
            "    ab      3 tries, built · last failed try: build failed",
            "    日本語  3 tries, built · last failed try: build failed",
        ]
    );
    let column = |row: &str| {
        let at = row.find("3 tries").unwrap();
        UnicodeWidthStr::width(&row[..at])
    };
    assert_eq!(column(rows[0]), column(rows[1]));
}

#[test]
fn a_narrow_width_wraps_the_detail_under_its_column() {
    let all = tries(&never_built(
        "pkg",
        "UnresolvableDeps: libfoo, libbar, libbaz, libqux, libquux",
    ));
    let block = summary_block(&all, 40);
    for line in block.lines() {
        assert!(UnicodeWidthStr::width(line) <= 40, "{line}");
    }
    assert!(block.lines().count() > 4, "{block}");
    let words: Vec<&str> = block.split_whitespace().collect();
    assert!(
        words
            .join(" ")
            .contains("unresolvable deps: libfoo, libbar, libbaz, libqux, libquux"),
        "{block}"
    );
}
