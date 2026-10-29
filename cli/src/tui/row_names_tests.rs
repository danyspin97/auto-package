use super::RowNames;

#[test]
fn a_dependency_label_is_not_a_request_name() {
    // `python-trantor` is built as a dependency of something else: its
    // label is `trantor`, but a retry must not resend `trantor` as a root
    // request, so no origin is recorded for it.
    let mut names = RowNames::default();
    names.note_label("python-trantor", "trantor");
    assert_eq!(names.label_of("python-trantor"), Some("trantor"));
    assert_eq!(names.origin_of("python-trantor"), None);
}

#[test]
fn a_root_request_is_not_a_label() {
    let mut names = RowNames::default();
    names.note_origin("python-foo", "foo");
    assert_eq!(names.origin_of("python-foo"), Some("foo"));
    assert_eq!(names.label_of("python-foo"), None);
}

#[test]
fn the_first_name_recorded_for_a_row_is_kept_in_each_map() {
    let mut names = RowNames::default();
    names.note_origin("python-foo", "foo");
    names.note_origin("python-foo", "other");
    names.note_label("python-foo", "foo");
    names.note_label("python-foo", "other");
    assert_eq!(names.origin_of("python-foo"), Some("foo"));
    assert_eq!(names.label_of("python-foo"), Some("foo"));
}
