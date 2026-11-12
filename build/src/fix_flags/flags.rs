use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixFlag {
    /// Tests the spec skips now, each one as the spec names it (`--deselect
    /// tests/a.py::t`, `-k "not slow"`, a `%{?with_...}` guard in `%check`).
    TestsSkipped { items: Vec<String> },
    /// `%check` was removed, or no test command is left in it.
    CheckRemoved,
    /// The package is built from the upstream wheel `wheel`, not from source.
    WheelSwitch { wheel: String },
    /// A fix turn answered `blocked`; `why` is the reason it gave and `run_id`
    /// the run whose turn it was (`None` in a state written before the field).
    Blocked {
        why: String,
        #[serde(default)]
        run_id: Option<String>,
    },
}

/// Longest a flag's text is kept on its one line.
pub(super) const LINE_CAP: usize = 240;

impl FixFlag {
    /// The one line that says it. The changelog, the request and the summary
    /// all print this line, so the wording is the same everywhere.
    pub fn line(&self) -> String {
        let text = match self {
            FixFlag::TestsSkipped { items } => format!("FLAG: tests skipped: {}", items.join("; ")),
            FixFlag::CheckRemoved => {
                "FLAG: %check removed or left without a test: the tests do not run".to_string()
            }
            FixFlag::WheelSwitch { wheel } => {
                format!("FLAG: built from the upstream wheel {wheel}, not from source")
            }
            FixFlag::Blocked { why, .. } => format!(
                "FLAG: a fix turn answered blocked: {}",
                ap_core::user_text::scrub_internal_refs(why)
            ),
        };
        one_line(&text)
    }
}

/// `text` on one line, its words single-spaced, cut to [`LINE_CAP`] characters.
pub(crate) fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LINE_CAP {
        return flat;
    }
    let cut: String = flat.chars().take(LINE_CAP).collect();
    format!("{}...", cut.trim_end())
}

/// Records a `blocked` answer of `run_id`.
pub fn add_blocked(st: &mut PkgState, run_id: &str, why: String) {
    // The flag says what the latest blocked turn answered: an earlier run's
    // answer, and this run's earlier one, give way to it.
    st.fix_flags
        .retain(|f| !matches!(f, FixFlag::Blocked { .. }));
    add(
        st,
        FixFlag::Blocked {
            why,
            run_id: Some(run_id.to_string()),
        },
    );
    st.fix_flags_run_id = Some(run_id.to_string());
}

/// Drops the `blocked` flags an earlier run than `run_id` recorded, for an
/// agent turn of `run_id` that was not blocked. A flag of `run_id` itself stays:
/// a turn of the run that was blocked is not undone by one that was not. True
/// when a flag was removed.
pub fn clear_blocked_of_earlier_runs(st: &mut PkgState, run_id: &str) -> bool {
    let before = st.fix_flags.len();
    st.fix_flags.retain(
        |f| !matches!(f, FixFlag::Blocked { run_id: r, .. } if r.as_deref() != Some(run_id)),
    );
    st.fix_flags.len() != before
}

/// Adds `flag` unless it is already recorded. Tests skipped by an earlier
/// commit and by this one are one flag listing both.
pub fn add(st: &mut PkgState, flag: FixFlag) {
    if let FixFlag::TestsSkipped { items } = &flag
        && let Some(FixFlag::TestsSkipped { items: have }) = st
            .fix_flags
            .iter_mut()
            .find(|f| matches!(f, FixFlag::TestsSkipped { .. }))
    {
        for item in items {
            if !have.contains(item) {
                have.push(item.clone());
            }
        }
        return;
    }
    if !st.fix_flags.contains(&flag) {
        st.fix_flags.push(flag);
    }
}

/// Adds the spec-derived flags `found` of a commit of `run_id`. Earlier flags
/// stay: a commit is compared with the OBS head, which already holds the
/// earlier commits' changes, so their flags would otherwise be lost. They are
/// dropped once the package is submitted (see [`clear_submitted`]).
pub fn settle(st: &mut PkgState, run_id: &str, found: Vec<FixFlag>) {
    for flag in found {
        add(st, flag);
    }
    st.fix_flags_run_id = Some(run_id.to_string());
}

/// The flags the package's last run recorded, for a reader outside the run.
/// Empty when they cannot be read; a reader that must not act on that uses
/// [`try_of_package`].
pub fn of_package(state_root: &std::path::Path, pkg: &str) -> Vec<FixFlag> {
    try_of_package(state_root, pkg).unwrap_or_default()
}

/// The flags of the package OBS calls `pkg`, or why they cannot be read: no
/// workspace for it, or a state file that is missing or corrupt.
pub fn try_of_package(state_root: &std::path::Path, pkg: &str) -> std::io::Result<Vec<FixFlag>> {
    let ws = Workspace::find_by_obs_name(state_root, pkg).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no build state for {pkg} in {}", state_root.display()),
        )
    })?;
    Ok(PkgState::read_strict(&ws)?.fix_flags)
}

/// Drops the flags of `pkg` once its change is submitted: the request text
/// carries them from then on. The workspace is the one [`try_of_package`]
/// reads.
pub fn clear_submitted(state_root: &std::path::Path, pkg: &str) -> std::io::Result<()> {
    let Some(ws) = Workspace::find_by_obs_name(state_root, pkg) else {
        return Ok(());
    };
    PkgState::update(&ws, |st| st.fix_flags.clear()).map(|_| ())
}
