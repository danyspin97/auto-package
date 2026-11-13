use super::*;

fn quiet() -> Emit<'static> {
    Emit::quiet()
}

#[test]
fn returns_output_of_a_command_that_finishes() {
    let out = capture_with_timeout(
        "sh",
        &["-c", "echo hi; echo err >&2"],
        Path::new("/"),
        quiet(),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        (out.code, out.stdout.trim(), out.stderr.trim()),
        (0, "hi", "err")
    );
}

#[test]
fn kills_a_command_that_outlives_the_limit() {
    let t = Instant::now();
    let err = capture_with_timeout(
        "sleep",
        &["30"],
        Path::new("/"),
        quiet(),
        Duration::from_millis(200),
    )
    .err()
    .expect("must time out");
    assert!(matches!(err, ProcessError::Timeout { .. }));
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn timeout_kills_grandchildren_holding_the_pipes() {
    let t = Instant::now();
    let err = capture_with_timeout(
        "sh",
        &["-c", "sleep 30 & wait"],
        Path::new("/"),
        quiet(),
        Duration::from_millis(200),
    )
    .err()
    .expect("must time out");
    assert!(matches!(err, ProcessError::Timeout { .. }));
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[test]
fn kill_process_group_sigkills_a_child_started_in_its_own_group() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    kill_process_group(child.id());
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

#[test]
fn unreadable_output_is_an_error_not_an_empty_success() {
    // The child exits at once but a background descendant in another
    // session keeps both pipes open, so neither stream is ever known.
    // The shell waits until the descendant has entered its new session
    // (it writes a marker from there) before it exits: otherwise the
    // group sweep after the exit can kill the descendant before its
    // setsid, closing the pipes, and the output is then complete.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("escaped");
    let script = "setsid sh -c 'touch \"$1\"; exec sleep 8' sh \"$1\" & \
                  until [ -e \"$1\" ]; do sleep 0.01; done; exit 0";
    let err = capture_with_timeout(
        "sh",
        &["-c", script, "sh", marker.to_str().unwrap()],
        Path::new("/"),
        quiet(),
        Duration::from_secs(20),
    )
    .err()
    .expect("must not be Ok");
    assert!(matches!(err, ProcessError::OutputLost { .. }));
}

#[test]
fn clean_exit_keeps_a_stream_that_was_read_in_full() {
    // stdout reaches EOF; only stderr is held open by the escaped sleep.
    // (the shell's own stdout pipe closes on exit, sleep's goes to null)
    let out = capture_with_timeout(
        "sh",
        &["-c", "echo hi; setsid sleep 8 >/dev/null & exit 0"],
        Path::new("/"),
        quiet(),
        Duration::from_secs(20),
    )
    .expect("complete stdout of a clean exit is returned");
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "hi\n");
}

/// Scratch files and the working directory of `gh`/`osc` live in a
/// directory only this user can enter, not at a predictable shared path.
#[test]
fn private_dir_is_a_fresh_mode_0700_directory_under_the_temp_dir() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir().unwrap();
    let meta = std::fs::symlink_metadata(dir).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    assert!(dir.starts_with(std::env::temp_dir()));
    // The same directory every time within one process.
    assert_eq!(dir, private_dir().unwrap());
}

#[test]
fn per_user_dir_is_verified_and_never_root() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let uid = unsafe { libc::getuid() };
    let dir = per_user_dir(tmp.path(), uid).unwrap();
    assert_eq!(dir, tmp.path().join(format!("auto-package-{uid}")));
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // Reused when it is still private.
    assert_eq!(per_user_dir(tmp.path(), uid).unwrap(), dir);
    // Someone else's uid, or loose permissions: refused.
    assert!(per_user_dir(tmp.path(), uid + 1).is_err());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(per_user_dir(tmp.path(), uid).is_err());
    // A missing parent is an error, not "/".
    assert!(per_user_dir(&tmp.path().join("missing"), uid).is_err());
}

#[test]
fn minimal_env_skips_non_utf8_values() {
    use std::os::unix::ffi::OsStringExt;
    // SAFETY: the name is unique to this test.
    unsafe {
        std::env::set_var(
            "LC_AP_TEST_NON_UTF8",
            std::ffi::OsString::from_vec(vec![b'a', 0xff]),
        );
    }
    let env = minimal_env(&[]);
    assert!(!env.contains_key("LC_AP_TEST_NON_UTF8"));
    unsafe { std::env::remove_var("LC_AP_TEST_NON_UTF8") };
}

#[test]
fn push_tail_keeps_only_the_last_cap_bytes() {
    let mut buf = VecDeque::new();
    push_tail(&mut buf, b"abcdef", 4);
    push_tail(&mut buf, b"gh", 4);
    assert_eq!(Vec::from(buf.clone()), b"efgh");
    push_tail(&mut buf, b"ij", 4);
    assert_eq!(Vec::from(buf), b"ghij");
}

#[test]
fn a_stream_larger_than_the_cap_keeps_its_end() {
    let mut buf = VecDeque::new();
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    for chunk in data.chunks(777) {
        push_tail(&mut buf, chunk, 1_000);
    }
    assert_eq!(Vec::from(buf), &data[data.len() - 1_000..]);
}

#[test]
fn large_output_does_not_deadlock() {
    let out = capture_with_timeout(
        "sh",
        &["-c", "head -c 2000000 /dev/zero | tr '\\0' x"],
        Path::new("/"),
        quiet(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(out.stdout.len(), 2_000_000);
}

#[test]
fn a_missing_working_directory_is_named_as_the_cause() {
    let dir = Path::new("/ap-test-missing-cwd/none");
    let err = capture_with_timeout("sh", &["-c", "true"], dir, quiet(), Duration::from_secs(10))
        .err()
        .expect("must fail to launch");
    let text = err.to_string();
    assert!(
        text.contains("working directory /ap-test-missing-cwd/none does not exist"),
        "{text}"
    );
}

#[test]
fn a_missing_program_is_still_named_as_the_program() {
    let err = capture_with_timeout(
        "ap-no-such-program-for-test",
        &["x"],
        Path::new("/"),
        quiet(),
        Duration::from_secs(10),
    )
    .err()
    .expect("must fail to launch");
    let text = err.to_string();
    assert!(
        text.starts_with("could not launch ap-no-such-program-for-test: "),
        "{text}"
    );
    assert!(!text.contains("working directory"), "{text}");
}
