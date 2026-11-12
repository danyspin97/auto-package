use super::*;

const MESSAGE: &str = "auto-package: Update to 2.14.0";
/// The source revision of the commit (the link's own revision, `_history`).
const SOURCE: &str = "5c67e9c82c4b4410c0331cd3900c7394";
/// The expanded revision the server's service run gives the package (`xsrcmd5`).
const EXPANDED: &str = "8cc13f95bfbd9469b56a99644611c3ac";

/// `_history` of a link whose commit (`SOURCE`) is followed by the server's
/// service run, which OBS commits under the `_service` user with no message.
fn history_with_service_run() -> String {
    format!(
        "<revisionlist>\n\
         <revision rev=\"4\" vrev=\"1\"><srcmd5>1bab7af08d87e68fc8a0bb2161fbab87</srcmd5><user>jdoe</user><time>1794000000</time><comment>auto-package: Update to 2.13.0</comment></revision>\n\
         <revision rev=\"5\" vrev=\"2\"><srcmd5>{SOURCE}</srcmd5><user>jdoe</user><time>1794239724</time><comment>{MESSAGE}</comment></revision>\n\
         <revision rev=\"6\" vrev=\"3\"><srcmd5>feedfeedfeedfeedfeedfeedfeedfeed</srcmd5><user>_service</user><time>1794239800</time><comment></comment></revision>\n\
         </revisionlist>"
    )
}

/// The expanded listing of the link after the commit: its own revision is
/// `SOURCE`, its expanded revision `EXPANDED`.
fn link_listing(own: &str, expanded: &str) -> String {
    format!(
        "<directory name=\"python-cramjam\" srcmd5=\"{expanded}\">\n  \
         <linkinfo project=\"devel:languages:python\" package=\"python-cramjam\" \
         srcmd5=\"1bab\" xsrcmd5=\"{expanded}\" lsrcmd5=\"{own}\"/>\n</directory>"
    )
}

#[test]
fn the_commit_is_found_by_its_user_and_message_not_as_the_newest_revision() {
    let source = commit_source(&history_with_service_run(), "jdoe", MESSAGE).unwrap();
    assert_eq!(source.srcmd5, SOURCE);
    assert_eq!(source.rev, "5");
}

#[test]
fn a_message_collapses_white_space_as_the_history_does() {
    let spaced = "auto-package:   Update  to 2.14.0";
    let source = commit_source(&history_with_service_run(), "jdoe", spaced).unwrap();
    assert_eq!(source.srcmd5, SOURCE);
}

#[test]
fn another_users_commit_with_the_same_message_is_not_this_commit() {
    assert!(commit_source(&history_with_service_run(), "someone", MESSAGE).is_none());
}

#[test]
fn a_link_commit_to_a_service_package_records_the_expanded_head() {
    let head = service_commit_head(
        &history_with_service_run(),
        &link_listing(SOURCE, EXPANDED),
        EXPANDED,
        "jdoe",
        MESSAGE,
    );
    // The expanded revision is what OBS reports as the head and what the
    // checkout records, not the source revision.
    assert_eq!(head.as_deref(), Ok(EXPANDED));
}

#[test]
fn a_non_link_service_package_records_its_directory_revision_when_it_is_the_source() {
    let listing = format!("<directory name=\"x\" srcmd5=\"{SOURCE}\"></directory>");
    assert_eq!(
        service_commit_head(
            &history_with_service_run(),
            &listing,
            SOURCE,
            "jdoe",
            MESSAGE
        ),
        Ok(SOURCE.to_string())
    );
}

#[test]
fn an_expanded_revision_that_is_not_the_source_of_the_commit_is_not_recorded() {
    // The package's own revision is another one: the link's base moved, so the
    // commit is not provably the package's.
    let other = "1bab7af08d87e68fc8a0bb2161fbab87";
    let why = service_commit_head(
        &history_with_service_run(),
        &link_listing(other, EXPANDED),
        EXPANDED,
        "jdoe",
        MESSAGE,
    )
    .unwrap_err();
    assert!(why.contains("source revision"), "{why}");
}

#[test]
fn a_checkout_not_holding_the_expanded_head_records_nothing() {
    let why = service_commit_head(
        &history_with_service_run(),
        &link_listing(SOURCE, EXPANDED),
        "015dbda0d164436c0ea7c10f5d4ae032",
        "jdoe",
        MESSAGE,
    )
    .unwrap_err();
    assert!(why.contains("the checkout holds"), "{why}");
}

#[test]
fn a_commit_missing_from_the_history_records_nothing() {
    let why = service_commit_head(
        &history_with_service_run(),
        &link_listing(SOURCE, EXPANDED),
        EXPANDED,
        "jdoe",
        "auto-package: Update to 9.9.9",
    )
    .unwrap_err();
    assert!(why.contains("no revision"), "{why}");
}

/// The restore guard compares OBS's expanded head with the recorded revisions,
/// so the recorded value must be the expanded one for the guard to accept it.
#[test]
fn the_recorded_expanded_revision_passes_the_restore_guard() {
    let head = service_commit_head(
        &history_with_service_run(),
        &link_listing(SOURCE, EXPANDED),
        EXPANDED,
        "jdoe",
        MESSAGE,
    )
    .unwrap();
    assert!(may_restore_over(&head, EXPANDED, &[head.clone()]));
}

/// A history with one commit whose comment is `message` as OBS stores it.
fn history_of(message: &str) -> String {
    format!(
        "<revisionlist><revision rev=\"5\" vrev=\"2\"><srcmd5>{SOURCE}</srcmd5>\
         <user>jdoe</user><time>1794239724</time><comment>{}</comment></revision></revisionlist>",
        crate::obs_xml::escape(message).replace('\'', "&apos;")
    )
}

#[test]
fn a_message_with_xml_special_characters_matches_its_escaped_comment() {
    let message = "auto-package: Fix \"quoted\" it's a & b <c> \u{e9}\u{4e2d}";
    let source = commit_source(&history_of(message), "jdoe", message).unwrap();
    assert_eq!(source.srcmd5, SOURCE);
}

#[test]
fn a_numeric_entity_in_the_comment_is_decoded() {
    let history = history_of("x").replace(">x<", ">say &#34;hi&#x22; &#233;<");
    let source = commit_source(&history, "jdoe", "say \"hi\" \u{e9}").unwrap();
    assert_eq!(source.rev, "5");
}

#[test]
fn a_message_cut_at_100_characters_next_to_a_quote_matches() {
    let message = format!("auto-package: {}\"{}", "a".repeat(84), "b");
    let cut: String = message.chars().take(100).collect();
    assert_eq!(cut.chars().count(), 100);
    assert!(commit_source(&history_of(&cut), "jdoe", &cut).is_some());
}
