//! What one other-architecture result says: its state, and for a failure the
//! line that names it, for a missing dependency the names OBS gives.

use crate::osc::BuildStatus;

/// The state of one `(repository, arch)` result of the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchState {
    Succeeded,
    /// The build failed; `detail` is its first error line when the log has one.
    Failed {
        detail: Option<String>,
    },
    /// Build dependencies cannot be resolved on this architecture; `detail`
    /// names them.
    Unresolvable {
        detail: Option<String>,
    },
    Broken,
    /// Excluded or disabled by the spec or the project: never a failure.
    Ignored,
    /// Still scheduled, blocked or building (or not reported yet).
    Pending(String),
}

impl ArchState {
    /// A failure on the architecture: the package does not build there.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            ArchState::Failed { .. } | ArchState::Unresolvable { .. } | ArchState::Broken
        )
    }

    /// Whether the result may still change, so the wait goes on.
    pub fn is_pending(&self) -> bool {
        matches!(self, ArchState::Pending(_))
    }

    /// The detail a failure or an unresolvable result carries, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            ArchState::Failed { detail } | ArchState::Unresolvable { detail } => detail.as_deref(),
            _ => None,
        }
    }

    /// The word the summary and the run log use for the state.
    pub fn label(&self) -> String {
        match self {
            ArchState::Succeeded => "succeeded".to_string(),
            ArchState::Failed { .. } => "failed".to_string(),
            ArchState::Unresolvable { .. } => "unresolvable".to_string(),
            ArchState::Broken => "broken".to_string(),
            ArchState::Ignored => "excluded".to_string(),
            ArchState::Pending(what) => format!("still {what}"),
        }
    }
}

/// Classify a result from its OBS status and its `<details>` text.
pub fn classify(status: &BuildStatus, details: Option<&str>) -> ArchState {
    match status {
        BuildStatus::Succeeded => ArchState::Succeeded,
        BuildStatus::Failed => ArchState::Failed { detail: None },
        BuildStatus::Unresolvable => {
            let missing = missing_from_details(details.unwrap_or(""));
            ArchState::Unresolvable {
                detail: (!missing.is_empty()).then(|| format!("missing {}", missing.join(", "))),
            }
        }
        BuildStatus::Broken => ArchState::Broken,
        BuildStatus::Excluded | BuildStatus::Disabled => ArchState::Ignored,
        BuildStatus::Scheduled => ArchState::Pending("scheduled".to_string()),
        BuildStatus::Blocked => ArchState::Pending("blocked".to_string()),
        BuildStatus::Building => ArchState::Pending("building".to_string()),
        BuildStatus::Unknown(s) => ArchState::Pending(s.clone()),
    }
}

/// The names an unresolvable result's details say are missing: the word after
/// each `nothing provides`, without repeats.
pub fn missing_from_details(details: &str) -> Vec<String> {
    const MARK: &str = "nothing provides ";
    let mut out: Vec<String> = Vec::new();
    for (at, _) in details.match_indices(MARK) {
        let rest = &details[at + MARK.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ',' && *c != ';')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The first line of a build log that names the failure: the first line that
/// reads like an error, else the last non-empty line. `None` for an empty log.
pub fn first_error_line(log: &str) -> Option<String> {
    let lines: Vec<&str> = log
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains("error") || lower.contains("nothing provides")
        })
        .or(lines.last())
        .map(|l| l.chars().take(200).collect())
}
