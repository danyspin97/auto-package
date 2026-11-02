use super::source_guard::{repeated_sources, source_value};

const BASE: &str = "Name: river\nSource0: river-%{version}.tar.gz\nSource1: river.sig\n";

#[test]
fn a_source_line_gives_its_value_and_a_comment_or_other_tag_does_not() {
    assert_eq!(
        source_value("Source7:        https://x.org/a.tar.gz"),
        Some("https://x.org/a.tar.gz")
    );
    assert_eq!(source_value("source12: a.tar.gz # note"), Some("a.tar.gz"));
    assert_eq!(source_value("# Source7: https://x.org/a.tar.gz"), None);
    assert_eq!(source_value("Patch3: fix.patch"), None);
    assert_eq!(source_value("SourceX: a"), None);
    assert_eq!(
        source_value("zig fetch --global-cache-dir vendor/ %{SOURCE7}"),
        None
    );
}

#[test]
fn a_new_source_is_not_a_repeat() {
    let after = format!("{BASE}Source7: https://codeberg.org/a/b/archive/c1.tar.gz\n");
    assert!(repeated_sources(BASE, &after).is_empty());
}

#[test]
fn a_source_already_in_the_spec_is_a_repeat() {
    let url = "https://codeberg.org/a/b/archive/c1.tar.gz";
    let before = format!("{BASE}Source7: {url}\n");
    let after = format!("{before}Source8: {url}\n");
    assert_eq!(repeated_sources(&before, &after), vec![url.to_string()]);
}

#[test]
fn a_source_added_twice_in_one_edit_is_a_repeat() {
    let url = "https://codeberg.org/a/b/archive/c1.tar.gz";
    let after = format!("{BASE}Source7: {url}\nSource8: {url}\n");
    assert_eq!(repeated_sources(BASE, &after), vec![url.to_string()]);
}

#[test]
fn a_source_the_spec_already_repeated_and_the_edit_leaves_alone_is_not_one() {
    let before = format!("{BASE}Source2: same.tar.gz\nSource3: same.tar.gz\n");
    let after = format!("{before}Patch0: fix.patch\n");
    assert!(repeated_sources(&before, &after).is_empty());
}

#[test]
fn a_replaced_url_is_not_a_repeat() {
    let before = format!("{BASE}Source7: https://x.org/a-c1.tar.gz\n");
    let after = format!("{BASE}Source7: https://x.org/a-c2.tar.gz\n");
    assert!(repeated_sources(&before, &after).is_empty());
}
