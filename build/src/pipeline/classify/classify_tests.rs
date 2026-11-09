//! The class of a failed source service: `Unclassified`, not an install
//! failure, on the lines of nvme-stas (Base_System run 20261104-132702).

use super::classify_failure;
use crate::log::{PathMapper, extract};
use ap_core::event::BuildFailure;

const SOURCE_SERVICE_FAILURE: &str = "\
[   12s] warning: %post(polkit-default-privs-1550+20261030.04c49cd-688.1.noarch) scriptlet failed, exit status 2
[   17s] Running build time source services...
[   17s] Error while moving from 'nvme-stas' to 'nvme-stas-3.1'
[   17s] service run failed for service 'tar'
";

#[test]
fn a_failed_source_service_is_not_an_install_failure() {
    let extracted = extract(
        SOURCE_SERVICE_FAILURE,
        &PathMapper::with_sdist_root("nvme-stas-3.1"),
    );
    match classify_failure(&extracted) {
        BuildFailure::Unclassified { raw } => {
            assert!(
                raw.contains("service run failed for service 'tar'"),
                "{raw}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// rpm's file-list check, run after `%check` ended the section run: the error
/// names no `Bad exit status`, and the file is attributed to `%check`.
#[test]
fn an_unpackaged_file_after_check_is_an_install_failure() {
    let log = "\
[   10s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.Ck1
[   12s] 12 passed in 2.1s
[   13s] Checking for unpackaged file(s): /usr/lib/rpm/check-files /home/abuild/rpmbuild/BUILDROOT/foo
[   13s] error: Installed (but unpackaged) file(s) found:
[   13s]    /usr/lib64/python3.13/site-packages/foo/data.txt
[   13s] RPM build errors:
[   13s]     Installed (but unpackaged) file(s) found:
[   13s]    /usr/lib64/python3.13/site-packages/foo/data.txt
";
    let extracted = extract(log, &PathMapper::with_sdist_root("foo-1.0"));
    assert_eq!(
        extracted.failure.as_ref().map(|f| f.phase.clone()),
        Some(crate::log::Phase::Section("%check".into()))
    );
    assert!(matches!(
        classify_failure(&extracted),
        BuildFailure::InstallFailed
    ));
}

/// A `%check` that exited non-zero is a check failure, even when its output
/// holds `ERROR` lines that look like post-build complaints.
#[test]
fn a_failing_check_section_is_still_a_check_failure() {
    let log = "\
[   10s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.Ck2
[   12s] ERROR tests/test_x.py::test_y - ValueError: bad input
[   12s] = 1 failed, 12 passed in 2.1s =
[   12s] error: Bad exit status from /var/tmp/rpm-tmp.Ck2 (%check)
";
    let extracted = extract(log, &PathMapper::with_sdist_root("foo-1.0"));
    assert!(matches!(
        classify_failure(&extracted),
        BuildFailure::CheckFailed
    ));
}
