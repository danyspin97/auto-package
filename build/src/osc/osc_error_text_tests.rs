use super::osc_error_text;

#[test]
fn a_traceback_is_reduced_to_the_exception_and_a_full_disk_is_named() {
    let stderr = "/x/rpms/a.rpm\nTraceback (most recent call last):\n  File \"/usr/bin/osc\", line 6, in <module>\n    o.write(buf)\nOSError: [Errno 28] No space left on device\n";
    let text = osc_error_text(stderr);
    assert!(
        text.starts_with("    o.write(buf)\nOSError: [Errno 28]"),
        "{text}"
    );
    assert!(text.contains("the disk is full"), "{text}");
    assert!(!text.contains("/usr/bin/osc"), "{text}");
    assert_eq!(osc_error_text("404 Not Found\n"), "404 Not Found");
}

#[test]
fn a_failed_service_keeps_what_it_printed_before_its_traceback() {
    let stderr = "service tar_scm failed:\nv5.0.4: No such revision\nTraceback (most recent call last):\n  File \"/usr/lib/obs/service/TarSCM/x.py\", line 1, in f\n    string = str(string, 'latin1')\nTypeError: decoding to str: need a bytes-like object, NoneType found\n";
    let text = osc_error_text(stderr);
    assert!(text.contains("v5.0.4: No such revision"), "{text}");
    assert!(text.contains("service tar_scm failed:"), "{text}");
    assert!(text.ends_with("NoneType found"), "{text}");
    assert!(!text.contains("TarSCM/x.py"), "{text}");
}

#[test]
fn a_traceback_that_is_not_a_service_failure_is_still_cut_to_two_lines() {
    let stderr = "some output\nTraceback (most recent call last):\n  File \"/usr/bin/osc\", line 6, in <module>\n    f()\nTypeError: boom\n";
    assert_eq!(osc_error_text(stderr), "    f()\nTypeError: boom");
}
