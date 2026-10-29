use super::set_version;

#[test]
fn plain_version_is_replaced_and_the_rest_kept() {
    let spec = "Name:           python-foo\nVersion:        0.1.0\nRelease:        0\n%build\n";
    let out = set_version(spec, "0.2.0").unwrap();
    assert_eq!(
        out,
        "Name:           python-foo\nVersion:        0.2.0\nRelease:        0\n%build\n"
    );
}

#[test]
fn a_version_macro_sets_its_define() {
    let spec = "%define package_version 1.0\nName: x\nVersion: %{package_version}\n";
    let out = set_version(spec, "2.0").unwrap();
    assert!(out.contains("%define package_version 2.0"), "{out}");
    assert!(out.contains("Version: %{package_version}"), "{out}");
}

#[test]
fn a_computed_version_is_refused() {
    assert!(set_version("Name: x\nVersion: %(echo 1)\n", "2").is_none());
}

#[test]
fn an_empty_version_gets_the_new_one_after_the_tag() {
    // Without a value, the version must not be inserted at column 0.
    let out = set_version("Name: x\nVersion:\nRelease: 0\n", "2.0").unwrap();
    assert_eq!(out, "Name: x\nVersion: 2.0\nRelease: 0\n");
}

#[test]
fn a_whitespace_only_version_keeps_the_tag_padding() {
    let out = set_version("Name: x\nVersion:    \nRelease: 0\n", "2.0").unwrap();
    assert_eq!(out, "Name: x\nVersion:    2.0\nRelease: 0\n");
    // The normal case is untouched by the empty-value branch.
    let out = set_version("Name: x\nVersion:    1.0\n", "2.0").unwrap();
    assert_eq!(out, "Name: x\nVersion:    2.0\n");
}
