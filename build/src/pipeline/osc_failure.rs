//! The failures of an `osc` call as a package ends: an error with a remedy is
//! an environment failure that keeps it; the rest read as they always did.

use super::*;

impl RunCtx<'_> {
    /// A failure of an `osc` call: an environment failure when the error has a
    /// remedy, else transient when the network, OBS or this machine failed
    /// rather than the package.
    pub(super) fn failed_osc(&self, e: &osc::Error) -> PipelineResult {
        if let Some(remedy) = e.remedy() {
            return self.failed_remedy(e.to_string(), remedy);
        }
        if e.is_disk_full() {
            return self.failed_as(FailureKind::DiskFull, e.to_string());
        }
        self.failed_text(e.is_transient(), e.to_string())
    }

    /// As [`Self::failed_text`] for an `osc` call's error, except that an error
    /// with a remedy is an environment failure that keeps it. A full disk
    /// reads as transient here, as it always has in these callers.
    pub(super) fn failed_osc_text(&self, e: &osc::Error) -> PipelineResult {
        match e.remedy() {
            Some(remedy) => self.failed_remedy(e.to_string(), remedy),
            None => self.failed_text(e.is_transient(), e.to_string()),
        }
    }

    /// A failure of the machine: the package cannot run here until `remedy` is
    /// acted on. Kind `Environment`, never retried.
    pub(super) fn failed_remedy(
        &self,
        reason: String,
        remedy: ap_core::remedy::Remedy,
    ) -> PipelineResult {
        PipelineResult::Failed {
            rpm_name: self.rpm_name.clone(),
            reason,
            kind: FailureKind::Environment,
            remedy: Some(remedy),
            note: None,
        }
    }

    /// A failure with an optional remedy: `Environment` with it, else `Other`
    /// as [`Self::failed`].
    pub(super) fn failed_env(
        &self,
        reason: String,
        remedy: Option<ap_core::remedy::Remedy>,
    ) -> PipelineResult {
        match remedy {
            Some(remedy) => self.failed_remedy(reason, remedy),
            None => self.failed(reason),
        }
    }
}
