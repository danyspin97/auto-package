use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RiskFlag {
    /// A scriptlet the baseline does not have, by its header (`%post -n foo`).
    NewScriptlet { header: String },
    /// A host that a `Source`/`Patch` URL or the `_service` file names and the
    /// baseline does not.
    NewSourceHost { host: String },
    /// A line that expands a shell command (`%(`) or Lua (`%{lua`), added or
    /// changed.
    ShellExpansion { line: String },
    /// A `Requires(pre|post|preun|postun...)` or `Provides` line that the
    /// change added (`added`) or removed.
    ScriptDependency { added: bool, line: String },
    /// A patch that was added or changed and touches `path`, which neither
    /// the baseline's patches nor the source archive have.
    PatchOutsideSet { patch: String, path: String },
    /// A `<service>` the `_service` file gains.
    NewService { name: String },
    /// A scriptlet whose body changed under a header the baseline has.
    ChangedScriptlet { header: String },
    /// A part of the change that could not be analysed, and why.
    Unanalysed { what: String },
    /// A patch that was added or changed and changes more lines than
    /// [`REVIEW_PATCH_LINES`]: it needs a person's review before it is submitted.
    LargePatch { patch: String, lines: usize },
    /// The update has no upstream notes (none in the source archive, none on
    /// the forge): its `.changes` entry says only `Update to <version>`.
    NoUpstreamNotes { version: String },
    /// A version that moves three or more minor series in the same major
    /// (`3.4.13` to `3.9.7`, `3.4` to `3.9`): a person checks what changed.
    LargeJump { from: String, to: String },
    /// A patch the patch reviewer rewrote itself, not only judged: a person
    /// looks at it before it is submitted.
    ReviewerRewrote { patch: String },
}

impl RiskFlag {
    /// The one line that says it, the same wherever it is printed.
    pub fn line(&self) -> String {
        let text = match self {
            RiskFlag::NewScriptlet { header } => format!("RISK: new scriptlet: {header}"),
            RiskFlag::NewSourceHost { host } => format!("RISK: new source host: {host}"),
            RiskFlag::ShellExpansion { line } => format!("RISK: shell or Lua expansion: {line}"),
            RiskFlag::ScriptDependency { added, line } => {
                let change = if *added { "added" } else { "removed" };
                format!("RISK: {change} dependency line: {line}")
            }
            RiskFlag::PatchOutsideSet { patch, path } => format!(
                "RISK: {patch} touches {path}, which no existing patch and no file of the \
                 source archive has"
            ),
            RiskFlag::NewService { name } => format!("RISK: new source service: {name}"),
            RiskFlag::ChangedScriptlet { header } => {
                format!("RISK: changed scriptlet body: {header}")
            }
            RiskFlag::Unanalysed { what } => format!("RISK: not analysed: {what}"),
            RiskFlag::LargePatch { patch, lines } => format!(
                "RISK: {patch} changes {lines} lines (over {REVIEW_PATCH_LINES}), needs a review"
            ),
            RiskFlag::NoUpstreamNotes { version } => format!(
                "RISK: no upstream changes listed for {version}; read the sources for what changed"
            ),
            RiskFlag::LargeJump { from, to } => {
                format!("RISK: large jump {from} -> {to}, needs a review")
            }
            RiskFlag::ReviewerRewrote { patch } => {
                format!("RISK: {patch} was rewritten by the patch reviewer, needs a review")
            }
        };
        one_line(&text)
    }
}

/// The flags for the patches a reviewer rewrote (`names`) that the final change
/// still has: a patch dropped since is not flagged.
pub fn reviewer_rewrite_flags(names: &[String], fin: &Files) -> Vec<RiskFlag> {
    names
        .iter()
        .filter(|n| fin.patches.contains_key(*n))
        .map(|patch| RiskFlag::ReviewerRewrote {
            patch: patch.clone(),
        })
        .collect()
}

/// Adds the risks `found` of a commit of `run_id`. Earlier risks stay until
/// the package is submitted (see [`clear_submitted`]), for the same reason as
/// the fix-loop flags: a later commit is compared with the OBS head.
pub fn settle(st: &mut PkgState, run_id: &str, found: Vec<RiskFlag>) {
    for risk in found {
        if !st.risk_flags.contains(&risk) {
            st.risk_flags.push(risk);
        }
    }
    st.risk_flags_run_id = Some(run_id.to_string());
}

/// Records (`missing`) or clears the no-upstream-notes risk of an update to
/// `version`, as the recap of its `.changes` entry found no notes or some.
pub fn settle_no_upstream_notes(st: &mut PkgState, run_id: &str, version: &str, missing: bool) {
    st.risk_flags
        .retain(|r| !matches!(r, RiskFlag::NoUpstreamNotes { .. }));
    if missing {
        st.risk_flags.push(RiskFlag::NoUpstreamNotes {
            version: version.to_string(),
        });
    }
    st.risk_flags_run_id = Some(run_id.to_string());
}

/// Drops the risks of `pkg` once its change is submitted. The workspace is the
/// one [`try_of_package`] reads.
pub fn clear_submitted(state_root: &Path, pkg: &str) -> std::io::Result<()> {
    let Some(ws) = Workspace::find_by_obs_name(state_root, pkg) else {
        return Ok(());
    };
    crate::pipeline::review_marks::clear_reviewer_rewrites(&ws);
    PkgState::update(&ws, |st| st.risk_flags.clear()).map(|_| ())
}

/// The risk flags the package's last run recorded, for a reader outside the run.
/// Empty when they cannot be read; a reader that must not act on that uses
/// [`try_of_package`].
pub fn of_package(state_root: &Path, pkg: &str) -> Vec<RiskFlag> {
    try_of_package(state_root, pkg).unwrap_or_default()
}

/// The risk flags of the package OBS calls `pkg`, or why they cannot be read:
/// no workspace for it, or a state file that is missing or corrupt.
pub fn try_of_package(state_root: &Path, pkg: &str) -> std::io::Result<Vec<RiskFlag>> {
    let ws = Workspace::find_by_obs_name(state_root, pkg).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no build state for {pkg} in {}", state_root.display()),
        )
    })?;
    Ok(PkgState::read_strict(&ws)?.risk_flags)
}

/// The lines of the package's risk flags, one each, as the readers print them.
pub fn lines_of_package(state_root: &Path, pkg: &str) -> Vec<String> {
    of_package(state_root, pkg)
        .iter()
        .map(RiskFlag::line)
        .collect()
}
