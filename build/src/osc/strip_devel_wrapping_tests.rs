use super::*;

#[test]
fn strips_lib_prefix_and_devel_suffix() {
    assert_eq!(strip_devel_wrapping("libtrantor-devel"), "trantor");
}

#[test]
fn a_name_that_is_only_lib_and_devel_is_kept_whole() {
    assert_eq!(strip_devel_wrapping("lib-devel"), "lib-devel");
    assert_eq!(strip_devel_wrapping("-devel"), "-devel");
}

#[test]
fn python_devel_strips_to_python() {
    assert_eq!(strip_devel_wrapping("python3-devel"), "python3");
}

#[test]
fn strips_lib_prefix_and_dev_suffix() {
    assert_eq!(strip_devel_wrapping("libfoo-dev"), "foo");
}

#[test]
fn strips_devel_suffix_without_lib_prefix() {
    assert_eq!(strip_devel_wrapping("apache-arrow-devel"), "apache-arrow");
}

#[test]
fn leaves_a_bare_lib_name_without_devel_suffix_untouched() {
    assert_eq!(strip_devel_wrapping("libyaml"), "libyaml");
}

#[test]
fn leaves_unrelated_names_untouched() {
    assert_eq!(strip_devel_wrapping("numpy"), "numpy");
}
