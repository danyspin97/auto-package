use super::*;

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn a_single_spec_is_the_spec_whatever_the_package_is_called() {
    let listing = files(&["_service", "pytorch.spec", "pytorch-2.4.tar.gz"]);
    assert_eq!(spec_file_name(&listing, "pytorch+2.4"), "pytorch.spec");
}

#[test]
fn a_single_spec_is_the_spec_whatever_its_name_is() {
    let listing = files(&["_service", "python-foo.spec"]);
    assert_eq!(spec_file_name(&listing, "foo"), "python-foo.spec");
    let listing = files(&["foo.spec", "foo-1.0.tar.gz"]);
    assert_eq!(spec_file_name(&listing, "foo:flavor"), "foo.spec");
}

#[test]
fn several_specs_fall_back_to_the_package_name() {
    let listing = files(&["a.spec", "b.spec"]);
    assert_eq!(spec_file_name(&listing, "pkg"), "pkg.spec");
    let listing = files(&["pkg.spec", "other.spec"]);
    assert_eq!(spec_file_name(&listing, "pkg"), "pkg.spec");
}

#[test]
fn no_spec_falls_back_to_the_package_name() {
    let listing = files(&["_service", "README"]);
    assert_eq!(spec_file_name(&listing, "pkg"), "pkg.spec");
    assert_eq!(spec_file_name(&[], "pkg"), "pkg.spec");
}
