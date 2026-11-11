//! How an `osc` error ends a package: an error with a remedy is an environment
//! failure that keeps the remedy; one without keeps the kind it always had.

use super::review_marks_tests::with_ctx;
use super::*;

fn outcome(result: PipelineResult) -> (FailureKind, String, Option<ap_core::remedy::Remedy>) {
    match result {
        PipelineResult::Failed {
            kind,
            reason,
            remedy,
            ..
        } => (kind, reason, remedy),
        _ => panic!("the package did not fail"),
    }
}

#[test]
fn a_missing_tool_ends_the_package_as_an_environment_failure_with_its_remedy() {
    with_ctx(|ctx, _| {
        let err = osc::Error::ToolMissing {
            tool: "cargo_vendor".into(),
            path: Some("/usr/lib/obs/service/cargo_vendor".into()),
        };
        let (kind, reason, remedy) = outcome(ctx.failed_osc(&err));
        assert_eq!(kind, FailureKind::Environment);
        assert_eq!(reason, err.to_string());
        assert_eq!(remedy, err.remedy());
        assert!(remedy.is_some());
    });
}

#[test]
fn an_unset_toolchain_ends_the_package_with_its_remedy_too() {
    with_ctx(|ctx, _| {
        let err = osc::Error::ToolchainUnset {
            tool: "cargo".into(),
        };
        let (kind, _, remedy) = outcome(ctx.failed_osc(&err));
        assert_eq!(kind, FailureKind::Environment);
        assert_eq!(
            remedy.map(|r| r.cause),
            Some(ap_core::remedy::Cause::ToolchainUnset {
                tool: "cargo".into()
            })
        );
    });
}

#[test]
fn an_error_without_a_remedy_keeps_its_kind_and_no_remedy() {
    with_ctx(|ctx, _| {
        let refused = osc::Error::OscFailed {
            cmd: "x".into(),
            code: 28,
            stderr: "timed out".into(),
        };
        let (kind, _, remedy) = outcome(ctx.failed_osc(&refused));
        assert_eq!(kind, FailureKind::Transient);
        assert_eq!(remedy, None);

        let full = osc::Error::Io(std::io::Error::from_raw_os_error(28));
        let (kind, _, remedy) = outcome(ctx.failed_osc(&full));
        assert_eq!(kind, FailureKind::DiskFull);
        assert_eq!(remedy, None);
    });
}

#[test]
fn the_service_text_path_keeps_a_remedy_and_reads_a_full_disk_as_transient() {
    with_ctx(|ctx, _| {
        let missing = osc::Error::ToolMissing {
            tool: "cargo_vendor".into(),
            path: None,
        };
        let (kind, _, remedy) = outcome(ctx.failed_osc_text(&missing));
        assert_eq!(kind, FailureKind::Environment);
        assert!(remedy.is_some());

        let full = osc::Error::Io(std::io::Error::from_raw_os_error(28));
        let (kind, _, remedy) = outcome(ctx.failed_osc_text(&full));
        assert_eq!(kind, FailureKind::Transient);
        assert_eq!(remedy, None);
    });
}
