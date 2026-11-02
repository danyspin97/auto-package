use super::service_root::has_services_root;

#[test]
fn a_services_root_is_found_after_comments_and_a_declaration() {
    assert!(has_services_root("<services/>\n"));
    assert!(has_services_root(
        "<?xml version=\"1.0\"?>\n<!-- x -->\n<services>\n</services>"
    ));
    assert!(!has_services_root(""));
    assert!(!has_services_root(" \n\t\n"));
    assert!(!has_services_root("<!-- nothing to run -->\n"));
    assert!(!has_services_root("<other/>"));
}
