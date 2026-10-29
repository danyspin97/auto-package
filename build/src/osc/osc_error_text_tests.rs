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
