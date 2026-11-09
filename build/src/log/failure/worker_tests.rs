//! OBS worker noise, on lines copied from the build.log of nvme-stas
//! (Base_System run 20261104-132702, 006-build), where the extractor blamed the
//! worker's polkit scriptlet for the failure.

use super::*;
use crate::log::PathMapper;
use crate::log::extract;

fn mapper() -> PathMapper {
    PathMapper::with_sdist_root("nvme-stas-3.1")
}

#[test]
fn the_polkit_scriptlet_noise_is_not_the_failure() {
    // Without the source service line, the polkit `%post` warning is the only
    // thing left that looks like a failure. It is not one.
    let raw = "\
[   12s] can't open /etc/polkit-1/rules.d/90-default-privs.rules.new: No such file or directory
[   12s] warning: %post(polkit-default-privs-1550+20261030.04c49cd-688.1.noarch) scriptlet failed, exit status 2
[   13s] warning: unused macro %foo in the spec
";
    let failure = extract(raw, &mapper()).failure.expect("a failure");
    assert_eq!(failure.located_by, "bare-error-line");
    assert!(
        failure.excerpt.contains("unused macro %foo"),
        "{}",
        failure.excerpt
    );
    assert!(!failure.excerpt.contains("polkit"), "{}", failure.excerpt);
    assert_ne!(failure.genre, Genre::PostBuildCheck);
}

#[test]
fn worker_noise_lines_are_recognised_and_real_warnings_are_not() {
    assert!(is_worker_noise(
        "can't open /etc/polkit-1/rules.d/90-default-privs.rules.new: No such file or directory"
    ));
    assert!(is_worker_noise(
        "warning: %post(polkit-default-privs-1550.noarch) scriptlet failed, exit status 2"
    ));
    assert!(!is_worker_noise(
        "warning: %post(foo-1.0.noarch) scriptlet failed"
    ));
    assert!(!is_worker_noise(
        "error: Bad exit status from /var/tmp/rpm-tmp.x (%install)"
    ));
}
