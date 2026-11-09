//! OBS worker noise and source-service failures, on lines copied from the
//! build.log of nvme-stas (Base_System run 20261104-132702, 006-build), where
//! the extractor blamed the worker's polkit scriptlet for a source service
//! that failed.

use super::*;
use crate::log::PathMapper;
use crate::log::extract;

/// The tail of that log, with its timestamps.
const NVME_STAS_TAIL: &str = "\
[   12s] polkit-default-privs-1550+20261030.04c49cd-688.1 ########################################
[   12s] can't open /etc/polkit-1/rules.d/90-default-privs.rules.new: No such file or directory
[   12s] warning: %post(polkit-default-privs-1550+20261030.04c49cd-688.1.noarch) scriptlet failed, exit status 2
[   16s] now finalizing build dir...
[   16s] ... running 11-hack_uname_version_to_kernel_version
[   16s] setting SOURCE_DATE_EPOCH_MTIME to 1793809897
[   17s] Running build time source services...
[   17s] Error while moving from 'nvme-stas' to 'nvme-stas-3.1'
[   17s] Current working directory: '/home/abuild/rpmbuild/SOURCES'
[   17s] service run failed for service 'tar'
[   17s] ### VM INTERACTION START ###
[   17s] [   13.206833][    T1] sysrq: Power Off
[   17s] ### VM INTERACTION END ###
[   17s] 
[   17s] i04-ch1c failed \"build nvme-stas.spec\" at Wed Nov  4 16:32:40 UTC 2026.
";

fn mapper() -> PathMapper {
    PathMapper::with_sdist_root("nvme-stas-3.1")
}

#[test]
fn a_failed_source_service_is_its_own_genre_with_the_real_lines() {
    let failure = extract(NVME_STAS_TAIL, &mapper())
        .failure
        .expect("a failure");
    assert_eq!(failure.genre, Genre::SourceService);
    assert_eq!(failure.located_by, "source-service");
    assert!(
        failure
            .excerpt
            .contains("Error while moving from 'nvme-stas' to 'nvme-stas-3.1'"),
        "{}",
        failure.excerpt
    );
    assert!(
        failure
            .excerpt
            .contains("service run failed for service 'tar'"),
        "{}",
        failure.excerpt
    );
    assert!(!failure.excerpt.contains("polkit"), "{}", failure.excerpt);
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
