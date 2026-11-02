//! The other-architecture results of a run: the rows of the summary's "Other
//! architectures" section.

use super::{ArchState, is_port_repo};

/// One checked `(repository, arch)` result of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchRow {
    pub pkg: String,
    pub repo: String,
    pub arch: String,
    pub state: ArchState,
    /// The same repository and arch fail in the baseline too (the origin, or
    /// for a port repository Factory), so the update did not cause it.
    pub baseline_fails: bool,
    /// The saved build log of a failure, if it was fetched.
    pub log: Option<String>,
}

/// Whether a result keeps the package from being submitted: a failure that
/// its baseline does not have too (`baseline_fails`). The one rule the summary
/// rows and the submit gate both apply.
pub fn blocks_submit(state: &ArchState, baseline_fails: bool) -> bool {
    state.is_failure() && !baseline_fails
}

impl ArchRow {
    /// Whether this row keeps the package from being submitted.
    pub fn blocks_submit(&self) -> bool {
        blocks_submit(&self.state, self.baseline_fails)
    }

    fn key(&self) -> (&str, &str, &str) {
        (&self.pkg, &self.repo, &self.arch)
    }
}

/// The lines of the "Other architectures" section: first one for each package
/// none of whose other results was built (see [`not_checked_lines`]), then one
/// per result that is not succeeded or skipped, one line each for a
/// `(pkg, repo, arch)`, first seen wins. A port result that is not testable
/// gets a line saying so. Empty when every checked result succeeded.
pub fn section_lines(rows: &[ArchRow]) -> Vec<String> {
    let mut seen: Vec<(&str, &str, &str)> = Vec::new();
    let mut out = not_checked_lines(rows);
    for row in rows {
        if matches!(row.state, ArchState::Succeeded) || row.state.is_skipped() {
            continue;
        }
        if seen.contains(&row.key()) {
            continue;
        }
        seen.push(row.key());
        let port = is_port_repo(&row.repo);
        let mut line = if let ArchState::NotTestable { .. } = row.state {
            format!("{} not testable on {}", row.pkg, row.arch)
        } else {
            format!(
                "{} {}/{}: {}",
                row.pkg,
                row.repo,
                row.arch,
                row.state.label()
            )
        };
        if let Some(detail) = row.state.detail() {
            line.push_str(&format!(": {detail}"));
        }
        if row.baseline_fails {
            let who = if port { "Factory" } else { "the origin" };
            line.push_str(&format!(" (also fails in {who})"));
        }
        if let ArchState::NotFinal(_) = row.state
            && port
        {
            line.push_str(&format!(
                " (results on {} can take hours: re-run `auto-package update` later to refresh)",
                row.arch
            ));
        }
        if let Some(log) = &row.log {
            line.push_str(&format!(" [log: {log}]"));
        }
        out.push(line);
    }
    out
}

/// One line for each package whose other-architecture results were all
/// skipped (disabled or excluded in the branch): nothing was built there, so
/// the check checked nothing and must not read as a pass.
pub fn not_checked_lines(rows: &[ArchRow]) -> Vec<String> {
    let mut pkgs: Vec<&str> = Vec::new();
    for row in rows {
        if !pkgs.contains(&row.pkg.as_str()) {
            pkgs.push(&row.pkg);
        }
    }
    pkgs.into_iter()
        .filter_map(|pkg| {
            let own: Vec<&ArchRow> = rows.iter().filter(|r| r.pkg == pkg).collect();
            if own.is_empty() || !own.iter().all(|r| r.state.is_skipped()) {
                return None;
            }
            let disabled = own.iter().all(|r| r.state == ArchState::Disabled);
            let why = if disabled {
                "the repositories are disabled in the branch"
            } else {
                "every result is disabled or excluded in the branch"
            };
            Some(format!(
                "{pkg}: not checked on the other architectures: {why} ({} results)",
                own.len()
            ))
        })
        .collect()
}
