use super::parse_sourceinfo_version;

#[test]
fn reads_the_version_element() {
    let xml = "<sourceinfo package=\"python-x\"><name>python-x</name>\
                   <version>0.1.14.post1</version><release>0</release></sourceinfo>";
    assert_eq!(
        parse_sourceinfo_version(xml).as_deref(),
        Some("0.1.14.post1")
    );
}

#[test]
fn no_version_element_is_none() {
    assert_eq!(parse_sourceinfo_version("<sourceinfo/>"), None);
    assert_eq!(parse_sourceinfo_version("<version></version>"), None);
}
