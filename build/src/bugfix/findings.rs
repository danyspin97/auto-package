//! What `fix --request` does with each finding of a review of a submit
//! request: the tool fixes it without a model, a turn fixes it, the ordinary
//! stages of the run fix it, OBS has to finish, a person decides, or nothing
//! is done. The routing is by id and fails closed: an id this table does not
//! list goes to a person.

use crate::request_review::{Finding, Origin, Severity, ids};

/// A finding of a review, as `fix --request` reads it: the same fields as
/// [`Finding`], owned, so a finding read back from a saved `review.json` is the
/// same kind of value as one a review of this run made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: Severity,
    pub origin: Origin,
    pub package: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub message: String,
}

impl From<&Finding> for ReviewFinding {
    fn from(f: &Finding) -> Self {
        ReviewFinding {
            id: f.id.to_string(),
            severity: f.severity,
            origin: f.origin,
            package: f.package.clone(),
            file: f.file.clone(),
            line: f.line,
            message: f.message.clone(),
        }
    }
}

/// A fix the tool makes on file texts, without a model. Each is a function of
/// `findings_fix`; one that cannot be sure refuses, and the finding then goes
/// to the route in [`MechanicalFix::fallback`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MechanicalFix {
    /// The Refresh bullets of the entry become one grouped bullet.
    RefreshBullets,
    /// A patch the request adds gets a bullet from its `Subject:`.
    PatchBullet,
    /// The old entries are put back as the target has them.
    RestoreOldEntries,
    /// `Update to <version>` is added to the entry.
    UpdateBullet,
    /// The entry header is written in the standard form.
    HeaderFormat,
    /// A bullet that only says a dependency changed is dropped.
    DropDependencyBullet,
    /// The `Update to` entry has no upstream notes under it: `lint` fills them in
    /// from the releases between the origin's version and the new one.
    UpstreamNotes,
    /// A leftover file of a tool or an editor is removed.
    RemoveLeftover,
}

/// What `fix --request` does with a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingRoute {
    /// Deterministic, no model.
    Tool(MechanicalFix),
    /// A `FixFindings` turn.
    Agent,
    /// The ordinary stages fix it: validation, the source fetch, the build rounds.
    Pipeline,
    /// Nothing to change: OBS has to finish.
    Wait,
    /// A person decides: why.
    Maintainer(&'static str),
    /// Notes and the agent's opinions: never fixed.
    NotActedOn,
}

impl MechanicalFix {
    /// Where a finding goes when its tool fix refuses.
    pub fn fallback(self) -> FindingRoute {
        match self {
            MechanicalFix::PatchBullet
            | MechanicalFix::UpdateBullet
            | MechanicalFix::UpstreamNotes
            | MechanicalFix::DropDependencyBullet => FindingRoute::Agent,
            MechanicalFix::RefreshBullets
            | MechanicalFix::RestoreOldEntries
            | MechanicalFix::HeaderFormat
            | MechanicalFix::RemoveLeftover => {
                FindingRoute::Maintainer("the tool could not make the fix with certainty")
            }
        }
    }
}

/// The reason of an id this table does not list.
pub const UNKNOWN_ID: &str = "not known to fix";

/// How a finding is handled. A note and any finding of the agent are never
/// acted on, whatever their id.
pub fn route(f: &ReviewFinding) -> FindingRoute {
    use FindingRoute::*;
    use MechanicalFix as M;
    if f.severity == Severity::Note || f.origin == Origin::Agent || f.id.starts_with("agent.") {
        return NotActedOn;
    }
    if f.id.starts_with("risk.") {
        return Maintainer("a change that needs a person's review by design");
    }
    match f.id.as_str() {
        ids::CHANGES_REFRESH_NOT_GROUPED => Tool(M::RefreshBullets),
        ids::CHANGES_PATCH_NOT_NAMED => Tool(M::PatchBullet),
        ids::CHANGES_OLD_ENTRIES_EDITED => Tool(M::RestoreOldEntries),
        ids::CHANGES_VERSION_MISMATCH => Tool(M::UpdateBullet),
        ids::CHANGES_HEADER_FORMAT => Tool(M::HeaderFormat),
        ids::CHANGES_DEPENDENCY_BULLET => Tool(M::DropDependencyBullet),
        ids::CHANGES_NO_UPSTREAM_NOTES => Tool(M::UpstreamNotes),
        ids::CHANGES_DATE_ORDER => {
            Maintainer("an entry's date is the author's record: the tool does not rewrite it")
        }
        ids::FILES_LEFTOVER | ids::SOURCE_OLD_ARCHIVE_KEPT => Tool(M::RemoveLeftover),
        ids::CHANGES_MISSING_ENTRY
        | ids::CHANGES_UPDATE_WITHOUT_VERSION_CHANGE
        | ids::CHANGES_FORBIDDEN_TEXT
        | ids::PATCH_HEADER
        | ids::PATCH_COMMENT
        | ids::PATCH_VERSION_IN_NAME
        | ids::PATCH_NOT_SOURCE_ONLY
        | ids::PATCH_SPEC_ROUTE
        | ids::RPMLINTRC_NON_FILTER
        | ids::RPMLINTRC_NO_REASON
        | ids::RPMLINTRC_SET_BADNESS
        | ids::SPEC_STYLE
        | ids::SPEC_HYGIENE
        | ids::SPEC_NO_CHECK
        | ids::SPEC_DROPPED_TESTS
        | ids::SPEC_VERSION_MACRO_UNEXPANDED
        | ids::SPEC_UNFINISHED
        | ids::FILES_UNREFERENCED => Agent,
        ids::SPEC_UNPARSABLE
        | ids::SOURCE_MISSING
        | ids::BUILD_FAILED
        | ids::BUILD_STALE
        | ids::BUILD_NO_RESULTS => Pipeline,
        ids::BUILD_PENDING => Wait,
        ids::SOURCE_SIGNATURE | ids::SOURCE_SIGNATURE_NOT_CHECKED | ids::SOURCE_KEYRING_CHANGED => {
            Maintainer("keys are a person's decision (`keyring accept`)")
        }
        ids::VERSION_BEHIND_UPSTREAM | ids::VERSION_NOT_CHECKED | ids::VERSION_REPOLOGY_NEWER => {
            Maintainer("a version is decided by `update --to-version`, not by a fix")
        }
        ids::REQUEST_COMPETING => Maintainer("another request for the package is open"),
        ids::REQUEST_NO_SOURCE_REV => {
            Maintainer("the request follows its source: --submit pins a new one")
        }
        ids::REQUEST_FINAL_STATE
        | ids::REQUEST_ACTION_NOT_REVIEWED
        | ids::REQUEST_ACTION_MALFORMED => {
            Maintainer("the request itself is not something a fix changes")
        }
        ids::REVIEW_INCOMPLETE => Maintainer("a check did not run: review it again"),
        ids::PATCH_UNREADABLE | ids::PATCH_DROPPED => {
            Maintainer("a patch the tool cannot read or a dropped patch needs a person")
        }
        ids::PATCH_PROVENANCE_MISSING
        | ids::PATCH_UPSTREAM_MERGED
        | ids::PATCH_UPSTREAM_RELEASED
        | ids::PATCH_UPSTREAM_MOVED
        | ids::PATCH_UPSTREAM_CLOSED => {
            Maintainer("the upstream request of a carried patch changed: a person checks it")
        }
        ids::SOURCE_UNEXPANDED => Maintainer("a source the tool cannot expand is not checked"),
        ids::BUILD_FAILS_IN_TARGET_TOO => {
            Maintainer("the target fails too: not this change's doing")
        }
        ids::BUILD_DISABLED_ARCH | ids::BUILD_NOT_TESTABLE => {
            Maintainer("the build cannot be tested here")
        }
        _ => Maintainer(UNKNOWN_ID),
    }
}

/// The findings of one request to fix in its branch package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingsJob {
    pub request_id: String,
    /// The user's branch project the request comes from.
    pub source_project: String,
    pub target_project: String,
    pub pkg: String,
    /// The branch's `Version:`; a fix never changes it.
    pub version: String,
    pub findings: Vec<(ReviewFinding, FindingRoute)>,
    /// The target's newest `.changes` text, `None` for a package it lacks.
    pub target_changes: Option<String>,
    /// The file names of the target package.
    pub target_files: Vec<String>,
    /// Ties `fix-end.json` to this run.
    pub nonce: String,
}

/// Why a request may not be superseded after a re-check: the finding that
/// stops it, with the message that says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A blocker a maintainer must resolve; the message names it.
    NeedsMaintainer(String),
    /// Findings nobody resolved; the message names them.
    Unresolved(String),
}

impl Refusal {
    /// What the refusal says, for the person who ran the command.
    pub fn message(&self) -> &str {
        match self {
            Refusal::NeedsMaintainer(message) | Refusal::Unresolved(message) => message,
        }
    }
}

/// Why the request must not be superseded, after a re-check left `after`.
/// `None` when every finding is resolved or is a person's to decide (a
/// warning with a maintainer route). A note and a finding of the agent never
/// stop it.
pub fn not_submittable(after: &[ReviewFinding]) -> Option<Refusal> {
    let mut maintainer_blocker = None;
    let mut unresolved = Vec::new();
    for f in after {
        let r = route(f);
        if f.severity == Severity::Note || matches!(r, FindingRoute::NotActedOn) {
            continue;
        }
        match r {
            FindingRoute::Maintainer(_) if f.severity == Severity::Blocker => {
                maintainer_blocker = Some(f);
            }
            FindingRoute::Maintainer(_) => {}
            _ => unresolved.push(f),
        }
    }
    if let Some(f) = maintainer_blocker {
        return Some(Refusal::NeedsMaintainer(format!(
            "a blocker needs a maintainer: {} {}: nothing was submitted; the branch keeps the fixes",
            f.id,
            f.file.as_deref().unwrap_or_default()
        )));
    }
    if !unresolved.is_empty() {
        let ids: Vec<&str> = unresolved.iter().map(|f| f.id.as_str()).collect();
        return Some(Refusal::Unresolved(format!(
            "{} finding(s) are not resolved ({}): nothing was submitted; the branch keeps the fixes",
            unresolved.len(),
            ids.join(", ")
        )));
    }
    None
}
