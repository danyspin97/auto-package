use super::*;

#[test]
fn a_patch_url_is_named_by_its_fragment_for_the_risk_flags() {
    let spec = "Name: easydev\nVersion: 0.12\nPatch:  https://github.com/cokelaer/easydev/commit/8cf50c9177936e771e158ea9b249b4aa5e2cc1ee.patch#/fix-version.patch\nPatch1: https://h/x/b.patch\n";
    assert_eq!(patch_names(spec), vec!["fix-version.patch", "b.patch"]);
}
