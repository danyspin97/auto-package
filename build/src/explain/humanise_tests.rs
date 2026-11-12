use super::humanise::humanise;

#[test]
fn camel_case_failure_kinds_read_as_lower_case_words() {
    let table = [
        ("InstallFailed", "install failed"),
        ("BuildFailed", "build failed"),
        ("PrepFailed", "prep failed"),
        ("CheckFailed", "check failed"),
        ("FilesNotOwned", "files not owned"),
        ("NoChange", "no change"),
        ("RpmParseError", "rpm parse error"),
        ("UnresolvableDeps", "unresolvable deps"),
        ("SRPMLintFailed", "SRPM lint failed"),
        ("OBSTimeout", "OBS timeout"),
        ("Python3Failed", "python3 failed"),
    ];
    for (raw, want) in table {
        assert_eq!(humanise(raw), want, "{raw}");
    }
}

#[test]
fn a_single_word_is_left_as_it_is() {
    for word in [
        "Timeout",
        "Unclassified",
        "build",
        "SRPM",
        "OBS",
        "a",
        "Tarball",
        "123",
    ] {
        assert_eq!(humanise(word), word);
    }
}

#[test]
fn an_empty_text_stays_empty() {
    assert_eq!(humanise(""), "");
}

#[test]
fn only_the_leading_word_changes_and_the_rest_is_kept() {
    assert_eq!(
        humanise("RpmParseError at line 3: bad tag"),
        "rpm parse error at line 3: bad tag"
    );
    assert_eq!(
        humanise("UnresolvableDeps: libfoo, LibBar"),
        "unresolvable deps: libfoo, LibBar"
    );
    assert_eq!(humanise("Unclassified: raw text"), "Unclassified: raw text");
}

#[test]
fn free_text_that_starts_with_a_plain_word_is_not_changed() {
    for text in [
        "install failed",
        "waiting for: foo, bar",
        "failed, no classification recorded",
        "disk full: no space left",
        "Tarball missing from the checkout",
    ] {
        assert_eq!(humanise(text), text);
    }
}
