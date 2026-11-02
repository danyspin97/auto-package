use super::{arch_excluded_reason, exclusive_arch_excluding};

const LSVPD: &str = "Name: lsvpd\nVersion: 1\nExclusiveArch:  ppc ppc64 ppc64le\n%description\nx\n";

#[test]
fn a_power_only_package_is_excluded_on_x86_64() {
    assert_eq!(
        exclusive_arch_excluding(LSVPD, "x86_64"),
        Some(vec!["ppc".into(), "ppc64".into(), "ppc64le".into()])
    );
    assert_eq!(exclusive_arch_excluding(LSVPD, "ppc64le"), None);
}

#[test]
fn macros_conditionals_and_the_tool_switch_are_left_alone() {
    for spec in [
        "ExclusiveArch: %{ix86} ppc\n",
        "%ifarch ppc\nExclusiveArch: ppc\n%endif\n",
        "ExclusiveArch: do_not_build\n",
        "Name: x\n",
    ] {
        assert_eq!(exclusive_arch_excluding(spec, "x86_64"), None, "{spec}");
    }
    assert_eq!(
        exclusive_arch_excluding("ExclusiveArch: i586\n", "i686"),
        None
    );
}

#[test]
fn a_line_in_a_subpackage_does_not_count() {
    let spec = "Name: x\n%package devel\nExclusiveArch: ppc\n";
    assert_eq!(exclusive_arch_excluding(spec, "x86_64"), None);
}

#[test]
fn the_reason_names_the_arch_tag_that_excludes_the_build() {
    assert_eq!(
        arch_excluded_reason(LSVPD, "x86_64").as_deref(),
        Some("ExclusiveArch excludes x86_64 (the spec allows ppc ppc64 ppc64le)")
    );
    assert_eq!(arch_excluded_reason(LSVPD, "ppc64le"), None);
    let no_x86 = "Name: x\nExcludeArch: x86_64 aarch64\n%description\nx\n";
    assert_eq!(
        arch_excluded_reason(no_x86, "x86_64").as_deref(),
        Some("ExcludeArch names x86_64")
    );
    assert_eq!(arch_excluded_reason(no_x86, "ppc64le"), None);
    // i586 in the list covers an i686 build, as for ExclusiveArch.
    assert_eq!(
        arch_excluded_reason("ExcludeArch: i586\n", "i686").as_deref(),
        Some("ExcludeArch names i686")
    );
}

#[test]
fn arch_tags_left_to_the_build_are_no_reason() {
    for spec in [
        "ExcludeArch: %{ix86}\n",
        "%ifarch s390x\nExcludeArch: x86_64\n%endif\n",
        "Name: x\n%package devel\nExcludeArch: x86_64\n",
    ] {
        assert_eq!(arch_excluded_reason(spec, "x86_64"), None, "{spec}");
    }
}
