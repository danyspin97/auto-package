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
