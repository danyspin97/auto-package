use super::*;

const SPEC: &str =
    "Name: fwupd-efi\nVersion: 1.9\nSource0: x.tar.gz\nSource1: fwupd-efi-rpmlintrc\n";

#[test]
fn the_rpmlintrc_the_spec_names_is_tidied_and_the_rest_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let text = "from Config import *\n\n# reason: r\naddFilter(\"a\")\n";
    std::fs::write(dir.path().join("fwupd-efi-rpmlintrc"), text).unwrap();
    // Not named by the spec: lint reports it, and it is never edited.
    std::fs::write(dir.path().join("other-rpmlintrc"), text).unwrap();
    let fixes = rpmlintrc_fixes(dir.path(), SPEC).unwrap();
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].name, "fwupd-efi-rpmlintrc");
    assert_eq!(fixes[0].before, text);
    assert_eq!(fixes[0].after, "# reason: r\naddFilter(\"a\")\n");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("a-rpmlintrc"), "addFilter(\"x\")\n").unwrap();
    let clean = rpmlintrc_fixes(other.path(), "Name: a\nVersion: 1\nSource0: a.tar.gz\n");
    assert!(clean.unwrap().is_empty());
}

#[test]
fn a_named_rpmlintrc_the_checkout_lacks_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    assert!(rpmlintrc_fixes(dir.path(), SPEC).unwrap().is_empty());
}
