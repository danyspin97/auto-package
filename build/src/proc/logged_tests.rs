use super::*;

fn run(script: &str, log: &Path, timeout: Duration, grace: Duration) -> Option<i32> {
    run_logged(
        "sh",
        &["-c", script],
        Path::new("/"),
        log,
        Emit::quiet(),
        timeout,
        grace,
    )
    .unwrap()
}

#[test]
fn a_finished_child_leaves_its_output_and_code_in_the_log() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("build.log");
    let code = run(
        "echo out; echo err >&2; exit 3",
        &log,
        Duration::from_secs(30),
        Duration::from_secs(1),
    );
    assert_eq!(code, Some(3));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("out") && text.contains("err"), "{text}");
}

#[test]
fn a_child_past_the_timeout_is_stopped_and_its_partial_log_kept() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("build.log");
    let start = std::time::Instant::now();
    let code = run(
        "echo started; sleep 60",
        &log,
        Duration::from_millis(300),
        Duration::from_millis(500),
    );
    assert_eq!(code, None);
    assert!(start.elapsed() < Duration::from_secs(20));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("started"), "{text}");
}
