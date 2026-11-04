//! Signatures and the keyring of the request, through the pipeline's own
//! check of the signed sources.

use super::downloads::Downloads;
use super::gather::ActionInputs;
use super::{Finding, Severity, ids};
use crate::pipeline::{SignatureCheck, check_signed_sources};

/// The finding of one signature check result.
pub(crate) fn signature_result(package: &str, check: SignatureCheck) -> Option<Finding> {
    // The pipeline's text ends in what it did with OBS, which a review never does.
    let plain = |text: String| {
        text.replace(" Nothing was sent to OBS", "")
            .replace(". .", ".")
    };
    match check {
        SignatureCheck::Ok => None,
        SignatureCheck::Refused(text) => Some(Finding::check(
            ids::SOURCE_SIGNATURE,
            Severity::Blocker,
            package,
            &plain(text),
        )),
        SignatureCheck::Skipped(text) => Some(Finding::check(
            ids::SOURCE_SIGNATURE_NOT_CHECKED,
            Severity::Note,
            package,
            &plain(text),
        )),
    }
}

/// The signature findings of the downloaded files, and the keyring change.
pub fn signature_findings(a: &ActionInputs, downloads: &Downloads) -> Vec<Finding> {
    let package = a.target.package.as_str();
    let mut out = Vec::new();
    let has_signature = downloads.fetched.iter().any(|n| {
        crate::pipeline::SIGNATURE_SUFFIXES
            .iter()
            .any(|s| n.ends_with(s))
    });
    if has_signature
        && let Some(f) = signature_result(package, check_signed_sources(&downloads.dir))
    {
        out.push(f);
    }
    out.extend(keyring_findings(a));
    out
}

/// A keyring that the request adds or changes: its keys are a maintainer's
/// decision (`keyring accept`).
pub fn keyring_findings(a: &ActionInputs) -> Vec<Finding> {
    a.files
        .added
        .iter()
        .chain(&a.files.changed)
        .filter(|n| n.ends_with(".keyring"))
        .map(|n| {
            Finding::check(
                ids::SOURCE_KEYRING_CHANGED,
                Severity::Warning,
                &a.target.package,
                &format!(
                    "{n} differs from the target's: keys in a keyring are a maintainer's decision"
                ),
            )
            .in_file(n, None)
        })
        .collect()
}
