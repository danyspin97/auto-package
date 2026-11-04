use super::*;

const COMMENT: &str =
    "# PATCH-FIX-UPSTREAM foo-CVE-2025-1234.patch bsc#1234567 CVE-2025-1234 -- check the length";
const FILE: &str = "foo-CVE-2025-1234.patch";

fn spec(sources: &str, prep: &str) -> String {
    format!("Name:           foo\nVersion:        1.0\n{sources}\n%prep\n{prep}\n%build\n")
}

#[test]
fn the_patch_goes_after_the_last_patch_with_the_next_number() {
    let s = spec(
        "Source0:        foo-1.0.tar.gz\n# PATCH-FIX-OPENSUSE a.patch\nPatch0:         a.patch\nPatch7:         b.patch\nBuildRequires:  gcc",
        "%autosetup -p1",
    );
    let out = spec_adding_patch(&s, COMMENT, FILE).unwrap();
    assert!(
        out.contains(&format!(
            "Patch7:         b.patch\n{COMMENT}\nPatch8:         {FILE}\nBuildRequires"
        )),
        "{out}"
    );
    assert!(crate::backport::spec_applies(&out, FILE));
}

#[test]
fn without_a_patch_it_goes_after_the_last_source() {
    let s = spec(
        "Source0:        foo-1.0.tar.gz\nSource1:        foo.keyring\nBuildRequires:  gcc",
        "%autosetup",
    );
    let out = spec_adding_patch_ok(&s);
    assert!(
        out.contains(&format!(
            "Source1:        foo.keyring\n{COMMENT}\nPatch0:         {FILE}\nBuildRequires"
        )),
        "{out}"
    );
}

fn spec_adding_patch_ok(s: &str) -> String {
    spec_adding_patch(s, COMMENT, FILE).unwrap()
}

#[test]
fn the_value_is_aligned_like_its_neighbour_and_a_short_column_gets_one_space() {
    let narrow =
        "Name: foo\nVersion: 1.0\nSource: foo.tar.gz\nPatch0: a.patch\n%prep\n%autosetup -p1\n";
    let out = spec_adding_patch_ok(narrow);
    assert!(
        out.contains(&format!("\n{COMMENT}\nPatch1: {FILE}\n")),
        "{out}"
    );
}

#[test]
fn a_spec_with_only_patch_lines_is_left_to_the_agent() {
    let s = spec(
        "Source0:        foo-1.0.tar.gz\nPatch0:         a.patch",
        "%setup -q\n%patch -P 0 -p1",
    );
    let e = spec_adding_patch(&s, COMMENT, FILE).unwrap_err();
    assert!(e.contains("agent"), "{e}");
}

#[test]
fn a_spec_with_no_source_has_no_place() {
    assert!(spec_adding_patch("Name: foo\n%prep\n%autosetup\n", COMMENT, FILE).is_err());
}

#[test]
fn the_trailing_newline_is_kept() {
    let s = spec("Source0: foo.tar.gz", "%autosetup");
    assert!(spec_adding_patch_ok(&s).ends_with('\n'));
    let bare = s.trim_end().to_string();
    assert!(!spec_adding_patch_ok(&bare).ends_with('\n'));
}
