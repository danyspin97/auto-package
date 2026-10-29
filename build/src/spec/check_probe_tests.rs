use super::check_is_file_probe_only;
use crate::spec::checks::*;

#[test]
fn only_file_probes_are_weak() {
    let weak = "%check\n%python_expand test -e %{buildroot}%{$python_sitearch}/x/y.so\n\n%files\n";
    assert!(check_is_file_probe_only(weak));
    let real = "%check\n%python_expand test -e a\n%pytest_arch tests/\n";
    assert!(!check_is_file_probe_only(real));
    let import = "%check\n%python_expand $python -c 'import x'\n";
    assert!(!check_is_file_probe_only(import));
    assert!(!check_is_file_probe_only("%install\nx\n"));
}
