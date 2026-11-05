//! Upstream provenance of carried patches: where a patch comes from (the
//! upstream request or commit it copies), and what the forge says about that
//! request. Reading the origin needs no network; see [`detect`].

mod detect;
#[cfg(test)]
mod detect_tests;
mod header;
#[cfg(test)]
mod header_tests;
mod reason;

pub use detect::{OriginInputs, OriginRead, classify_url, read_origin};
pub use header::{Claim, spec_with_comment, with_header};
pub use reason::{cut, fold_field, reason_from_changes, reason_of, sanitize};

/// What kind of upstream change a patch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    /// A pull request on GitHub, or a pull request of a Gitea-style forge.
    Pr,
    /// A merge request on GitLab.
    Mr,
    /// A single commit.
    Commit,
}

/// Where a patch comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchOrigin {
    /// The canonical web page: `https://github.com/fwupd/fwupd-efi/pull/106`,
    /// `https://gitlab.com/g/p/-/merge_requests/7`,
    /// `https://github.com/o/r/commit/<sha>`.
    pub url: String,
    pub kind: OriginKind,
    /// The repository, canonical https (`https://github.com/fwupd/fwupd-efi`).
    pub project: String,
    /// The request number (Pr, Mr).
    pub number: Option<u64>,
    /// The commit id (Commit), lowercase.
    pub commit: Option<String>,
    pub evidence: OriginEvidence,
}

/// Where an origin was read, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginEvidence {
    /// `Upstream:`, `References:`, `Origin:`, `Bug:`, `Forwarded:`, `Link:` or
    /// `Git-repo:` with `Git-commit:` in the patch file.
    PatchHeader,
    /// The comment block directly above the `PatchN:` line.
    SpecComment,
    /// The `PatchN:` value is itself the request's URL.
    PatchUrl,
    /// A `.changes` bullet that names the file.
    ChangesBullet,
    /// Only the file name (`106.patch`); needs corroboration before it is
    /// written anywhere.
    FileName,
}
