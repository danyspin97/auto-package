//! Real spec shapes the duplicate-comment lint must leave alone.

use super::duplicate_comments::duplicates;

const FIPS: &str = "# PATCH-FIX-FEDORA bsc#1 FIPS: add the module self tests";

#[test]
fn a_patch_tag_comment_above_each_patch_is_not_a_repeat() {
    let spec = format!("{FIPS}\nPatch1: a.patch\n{FIPS}\nPatch2: b.patch\n");
    assert!(duplicates(&spec).is_empty());
}

#[test]
fn a_comment_above_two_obsoletes_lines_is_not_a_repeat() {
    let c = "# Needed for clean upgrade from former openssl-1_1_0 packages";
    let spec = format!("{c}\nObsoletes: a\n{c}\nObsoletes: b\n");
    assert!(duplicates(&spec).is_empty());
}

#[test]
fn consecutive_url_lines_are_not_repeats() {
    let spec = "# https://lists.gnu.org/archive/html/bug-bash/2019-01/msg00070.html\n# https://lists.gnu.org/archive/html/bug-bash/2019-01/msg00071.html\n# https://lists.gnu.org/archive/html/bug-bash/2019-01/msg00072.html\n";
    assert!(duplicates(spec).is_empty());
}

#[test]
fn a_generic_unit_test_comment_is_not_a_restated_dependency() {
    let spec = "# The following packages are only required by the execution of the unit tests\nBuildRequires: a\n%check\n# Run the unit tests.\n%meson_test\n";
    assert!(duplicates(spec).is_empty());
}

#[test]
fn the_pleamar_pair_is_still_reported() {
    let spec = "# the text tests need at least one system font\nBuildRequires: dejavu-fonts\n\n%check\n# text tests panic with \"no default font found\" without a system font\n%{cargo_test}\n";
    assert_eq!(duplicates(spec).len(), 1);
}

#[test]
fn a_real_repeat_above_plain_lines_is_still_reported() {
    let c = "# the quick brown fox jumps over";
    let spec = format!("{c}\nBuildRequires: a\n{c}\nBuildRequires: b\n");
    assert_eq!(duplicates(&spec).len(), 1);
}
