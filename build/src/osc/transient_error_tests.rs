use super::*;

fn failed(code: i32, stderr: &str) -> Error {
    Error::OscFailed {
        cmd: "x".into(),
        code,
        stderr: stderr.into(),
    }
}

#[test]
fn network_obs_and_disk_failures_are_transient_and_package_failures_are_not() {
    assert!(failed(1, "HTTP Error 503: Service Unavailable").is_transient());
    assert!(failed(1, "502 Server Error: Bad Gateway for url").is_transient());
    assert!(
        failed(
            1,
            "fatal: unable to access: Could not resolve host: github.com"
        )
        .is_transient()
    );
    assert!(failed(28, "").is_transient());
    assert!(
        failed(
            -1,
            "gave up after 8 consecutive failed status polls; last error: x"
        )
        .is_transient()
    );
    assert!(failed(-1, "timed out after 7200s waiting for OBS build").is_transient());
    assert!(
        Error::Process(proc::ProcessError::Timeout {
            program: "osc".into(),
            secs: 1
        })
        .is_transient()
    );
    assert!(Error::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)).is_transient());
    assert!(
        Error::NotActionable(format!(
            "OBS reported p {DOWNLOAD_FAILED}: osc getbinaries failed"
        ))
        .is_transient()
    );
    // The package is at fault, or the run was stopped.
    assert!(!failed(1, "HTTP Error 404: Not Found").is_transient());
    assert!(!failed(1, "HTTP Error 403: Forbidden").is_transient());
    assert!(!failed(-1, "interrupted").is_transient());
    assert!(!Error::NotActionable(BROKEN_MESSAGE.into()).is_transient());
    assert!(
        !Error::UnresolvableDeps {
            names: vec![],
            detail: vec![]
        }
        .is_transient()
    );
    assert!(!Error::Io(std::io::Error::from(std::io::ErrorKind::NotFound)).is_transient());
}

#[test]
fn child_output_names_a_full_disk_in_any_case() {
    assert!(child_output_says_disk_full(
        "error: Bad exit status: cpio: write: No space left on device"
    ));
    assert!(child_output_says_disk_full("NO SPACE LEFT ON DEVICE"));
    assert!(!child_output_says_disk_full(
        "CMake Error: The LOCATION property may not be read from target \"png\""
    ));
}

#[test]
fn a_full_disk_is_typed_when_the_os_says_so_and_transient() {
    let from_os = Error::Io(std::io::Error::from_raw_os_error(28));
    assert!(from_os.is_disk_full() && from_os.is_transient());
    let from_child = Error::OscFailed {
        cmd: "up".into(),
        code: 1,
        stderr: "OSError: [Errno 28] No space left on device".into(),
    };
    assert!(from_child.is_disk_full() && from_child.is_transient());
    let other = Error::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
    assert!(!other.is_disk_full());
    assert!(!child_output_says_disk_full("version 2.0.0 is yanked"));
}

#[test]
fn transient_text_is_recognised() {
    assert!(text_is_transient("Connection reset by peer"));
    assert!(!text_is_transient("version 2.0.0 is yanked"));
    // A word of its own, not a name that contains it.
    assert!(text_is_transient("Error: Timeout"));
    assert!(text_is_transient("operation timed out"));
    assert!(!text_is_transient(
        "FAILED tests/test_net.py::test_timeout_is_set"
    ));
    assert!(!text_is_transient("unknown option --timeouts"));
}

#[test]
fn a_project_named_timeout_is_not_a_transient_failure() {
    // The header names the project; a 404 is the answer, not a timeout.
    let text = "osc meta prj home:alice:timeout failed (exit 1):\nHTTP Error 404: Not Found";
    assert!(!text_is_transient(text));
    assert!(lookup_says_absent(text, false));
    let pkg = "osc meta pkg home:alice:python-timeout-decorator foo failed (exit 1):\nHTTP Error 404: Not Found";
    assert!(!text_is_transient(pkg));
    assert!(lookup_says_absent(pkg, false));
    // Without a status the header is still not read as a timeout word.
    assert!(!text_is_transient("osc meta prj timeout failed (exit 1):"));
    // A 403 on a project we only read from is absent, as before.
    let forbidden = "osc meta prj home:alice:timeout failed (exit 1):\nHTTP Error 403: Forbidden";
    assert!(lookup_says_absent(forbidden, true));
    assert!(!lookup_says_absent(forbidden, false));
}

#[test]
fn a_gateway_timeout_or_a_timed_out_operation_is_transient() {
    assert!(text_is_transient("HTTP Error 504: Gateway Timeout"));
    assert!(!lookup_says_absent(
        "HTTP Error 504: Gateway Timeout",
        false
    ));
    assert!(text_is_transient("Operation timed out"));
    assert!(!lookup_says_absent("Operation timed out", false));
    // A bare timeout word in the error body still counts.
    assert!(text_is_transient(
        "osc meta prj home:alice:foo failed (exit 1):\nError: Timeout"
    ));
}

#[test]
fn a_build_log_with_a_latin1_byte_is_still_a_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    // `é` in Latin-1 is 0xE9 alone: not UTF-8, but a log all the same.
    fs::write(&path, b"caf\xE9 error: build failed\n").unwrap();
    assert!(broken_is_confirmed(&path));
    let text = usable_build_log(&path).unwrap();
    assert!(text.contains("error: build failed"), "{text}");
    assert!(text.starts_with("caf"), "{text}");
    // An empty or missing file is still an environment problem.
    let empty = dir.path().join("empty.txt");
    fs::write(&empty, b"").unwrap();
    assert!(matches!(usable_build_log(&empty), Err(Error::Io(_))));
    assert!(matches!(
        usable_build_log(&dir.path().join("missing.txt")),
        Err(Error::Io(_))
    ));
    assert!(!broken_is_confirmed(&empty));
}

#[test]
fn a_rich_dependency_keeps_names_that_start_with_a_digit() {
    assert_eq!(
        segment_package_names("(3proxy or python314-foo)"),
        vec!["3proxy".to_string(), "python314-foo".to_string()]
    );
    // The version after a comparison is no package, a name after `and` is.
    assert_eq!(
        segment_package_names("(foo >= 1.2 and bar)"),
        vec!["foo".to_string(), "bar".to_string()]
    );
    assert_eq!(
        segment_package_names("(python313-tomli >= 2.0.0 if python313-base < 3.11)"),
        vec!["python313-tomli".to_string(), "python313-base".to_string()]
    );
    // A glued `>=1.2` carries its version: the next word is a package.
    assert_eq!(
        segment_package_names("(foo>=1.2 or 2fa-tools)"),
        vec!["foo".to_string(), "2fa-tools".to_string()]
    );
    // A bare version without an operator is no name either.
    assert_eq!(
        segment_package_names("(foo 1.2.3 or bar)"),
        vec!["foo".to_string(), "bar".to_string()]
    );
}

#[test]
fn the_rich_dependency_names_reach_the_extraction() {
    let log = "nothing provides (3proxy or python314-foo)\n";
    assert_eq!(
        extract_unresolvable(log),
        vec!["3proxy".to_string(), "python314-foo".to_string()]
    );
}
