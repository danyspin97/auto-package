use super::*;

#[test]
fn provides_are_read_with_their_versions() {
    let xml = "<fileinfo filename=\"aml-devel.rpm\">\n  <provides>aml-devel = 1.0.0-lp161.14.1</provides>\n  <provides>pkgconfig(aml1) = 1.0.0</provides>\n  <provides>pkgconfig(x) &gt;= 2</provides>\n  <provides_ext dep=\"pkgconfig(aml1) = 1.0.0\"/>\n</fileinfo>";
    let texts = parse_provides(xml);
    assert_eq!(texts.len(), 3);
    assert_eq!(
        split_provide(&texts[1]),
        (
            "pkgconfig(aml1)".to_string(),
            Some(("=".to_string(), "1.0.0".to_string()))
        )
    );
    assert_eq!(
        split_provide(&texts[2]),
        (
            "pkgconfig(x)".to_string(),
            Some((">=".to_string(), "2".to_string()))
        )
    );
}

#[test]
fn an_unversioned_provide_has_no_version() {
    assert_eq!(
        split_provide("perl(Foo::Bar)"),
        ("perl(Foo::Bar)".to_string(), None)
    );
}

#[test]
fn search_answer_names_each_rpm_and_its_file() {
    let xml = "<collection matches=\"2\">\n  <binary name=\"glib2-devel\" project=\"X\" package=\"glib2\" repository=\"16.1\" version=\"2.80\" arch=\"x86_64\" filename=\"glib2-devel-2.80-1.x86_64.rpm\"/>\n  <binary name=\"glib2-devel\" project=\"Y\" repository=\"standard\" arch=\"noarch\" filename=\"glib2-devel-2.81-1.noarch.rpm\"/>\n</collection>";
    let found = parse_rpm_search(xml);
    assert!(!found.truncated);
    assert_eq!(found.rpms.len(), 2);
    assert_eq!(found.rpms[0].name, "glib2-devel");
    assert_eq!(found.rpms[0].package.as_deref(), Some("glib2"));
    assert_eq!(found.rpms[1].package, None);
}

#[test]
fn a_limited_search_answer_is_marked_truncated() {
    let xml = "<collection matches=\"4251\" limited=\"true\">\n  <binary name=\"a\" project=\"P\" repository=\"r\" arch=\"x86_64\" filename=\"a.rpm\"/>\n</collection>";
    assert!(parse_rpm_search(xml).truncated);
}

#[test]
fn search_expression_names_needle_arch_and_projects() {
    let path = rpm_search_path("glib", &["A".to_string(), "B".to_string()], "x86_64");
    assert!(path.starts_with("/search/published/binary/id?match="));
    assert!(path.contains("contains%28%40name%2C%27glib%27%29"));
    assert!(path.contains("%40project%3D%27A%27"));
    let open = rpm_search_path("glib", &[], "x86_64");
    assert!(!open.contains("project"));
}

#[test]
fn needles_with_operators_are_refused() {
    assert!(is_searchable_needle("sdbus-c++"));
    assert!(!is_searchable_needle("a'b"));
    assert!(!is_searchable_needle(""));
}

#[test]
fn a_repository_listing_names_each_rpm_without_its_version() {
    let xml = "<binarylist>\n  <binary filename=\"glib2-devel.rpm\" size=\"1\" mtime=\"2\"/>\n  <binary filename=\"glib2-devel-debuginfo.rpm\" size=\"1\" mtime=\"2\"/>\n  <binary filename=\"glib2-devel.rpm\" size=\"1\" mtime=\"3\"/>\n  <binary filename=\"notes.txt\" size=\"1\" mtime=\"2\"/>\n</binarylist>";
    assert_eq!(
        parse_repository_listing(xml),
        ["glib2-devel", "glib2-devel-debuginfo"]
    );
}

#[test]
fn a_file_under_its_architecture_directory_is_named_without_version() {
    let xml = "<binarylist>\n  <binary filename=\"x86_64/libdrm-devel-32bit-2.4.134-2.2.x86_64.rpm\" size=\"\" mtime=\"\"/>\n  <binary filename=\"noarch/0ad-data-0.28.0-1.2.noarch.rpm\" size=\"\" mtime=\"\"/>\n  <binary filename=\"libdrm-devel-32bit.rpm\" size=\"1\" mtime=\"2\"/>\n</binarylist>";
    assert_eq!(
        parse_repository_listing(xml),
        ["libdrm-devel-32bit", "0ad-data"]
    );
}

#[test]
fn an_empty_repository_lists_nothing() {
    assert!(parse_repository_listing("<binarylist/>").is_empty());
}

#[test]
fn a_large_listing_keeps_its_order_and_each_name_once() {
    // 30k binaries, every third one listed twice (the same file again, with
    // a newer mtime): each name comes out once, in listing order.
    let mut xml = String::from("<binarylist>\n");
    for i in 0..30_000 {
        xml.push_str(&format!(
            "  <binary filename=\"pkg{i}-devel.rpm\" size=\"1\" mtime=\"2\"/>\n"
        ));
        if i % 3 == 0 {
            xml.push_str(&format!(
                "  <binary filename=\"pkg{i}-devel.rpm\" size=\"1\" mtime=\"3\"/>\n"
            ));
        }
    }
    xml.push_str("</binarylist>");
    let started = std::time::Instant::now();
    let names = parse_repository_listing(&xml);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "a 30k listing took {:?}",
        started.elapsed()
    );
    assert_eq!(names.len(), 30_000);
    assert!(
        names
            .iter()
            .enumerate()
            .all(|(i, n)| *n == format!("pkg{i}-devel"))
    );
}
