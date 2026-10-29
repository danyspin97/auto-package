
use super::exclusive_arch_excluding;

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
