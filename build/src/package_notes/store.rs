use super::*;

/// Haiku is enough for summarising a journal, and it is the cheapest tier.
pub(super) const NOTES_MODEL: &str = "claude-haiku-5-5";

/// The most one notes turn can cost. The turn runs under this `--max-budget-usd`,
/// so it is also the upper bound the caller reserves before the call.
pub const NOTES_CAP_USD: f64 = 0.20;

/// Largest notes file the turn may write. A bigger result is refused and the
/// previous notes stay. The prompt asks for less (`TARGET_NOTES_LINES`) so a
/// notes file that has grown to the cap still has room to be updated.
pub(super) const MAX_NOTES_LINES: usize = 60;

pub(super) const TARGET_NOTES_LINES: usize = 45;

/// A retry after a refused update needs at least this much of the turn budget.
pub(super) const MIN_RETRY_USD: f64 = 0.03;

pub(super) const MAX_NOTES_BYTES: usize = 8 * 1024;

pub(super) const NOTES_TIMEOUT: Duration = Duration::from_secs(300);

/// Local git operations on the notes repository; they never touch a network.
pub(super) const GIT_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) static NOTES_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set where package notes are kept. Without it, notes are neither read nor
/// written, so library callers opt in explicitly.
pub fn set_notes_dir(dir: PathBuf) {
    let _ = NOTES_DIR.set(dir);
}

/// The notes file for `rpm_name`, or `None` when no notes directory is set.
///
/// A name that does not survive `sanitize_name` unchanged (`../x`, a slash)
/// could point outside the notes directory, so it gets no notes at all.
pub fn path_for(rpm_name: &str) -> Option<PathBuf> {
    if ap_core::run::sanitize_name(rpm_name) != rpm_name {
        return None;
    }
    NOTES_DIR
        .get()
        .map(|dir| dir.join(format!("{rpm_name}.md")))
}

/// How long a build waits for another build's update of the same notes. The
/// holder keeps the lock for the whole agent turn and for the git calls that
/// commit the file, so the wait must outlast that or a healthy update reads as
/// a timeout.
pub(super) const LOCK_WAIT: Duration =
    Duration::from_secs(NOTES_TIMEOUT.as_secs() + 5 * GIT_TIMEOUT.as_secs());

/// Exclusive lock on one package's notes, so two builds cannot both read the
/// same notes and have the later writer silently discard the earlier update.
/// An `flock(2)` on `<rpm-name>.md.lock`, held while the file stays open; the
/// kernel releases it if the build dies, so a crash leaves nothing to clear.
/// The lock file stays behind by design, one empty file per package: deleting a
/// locked file would let a second build lock a fresh one at the same path.
pub(super) struct NotesLock {
    _file: std::fs::File,
}

impl NotesLock {
    pub(super) fn acquire(notes_file: &Path) -> Result<Self, String> {
        Self::acquire_for(notes_file, LOCK_WAIT)
    }

    pub(super) fn acquire_for(notes_file: &Path, wait: Duration) -> Result<Self, String> {
        use ap_core::proc_lock::{LockError, lock_with_wait};
        let path = PathBuf::from(format!("{}.lock", notes_file.display()));
        match lock_with_wait(&path, wait) {
            Ok(file) => Ok(Self { _file: file }),
            Err(LockError::Held(_)) => Err(format!(
                "timed out waiting for {}: another build is updating these notes",
                path.display()
            )),
            Err(LockError::Io(e)) => Err(format!("cannot create {}: {e}", path.display())),
        }
    }
}

/// One quiet `claude --print --output-format json` call with the scrubbed
/// agent environment, shared by the notes update and the source lookup. It
/// always runs with `--permission-mode default --setting-sources project`: path
/// rules in `tools` are only enforced in that mode, and user settings could
/// otherwise widen them.
pub fn claude_print_json(
    model: &str,
    tools: &str,
    budget_usd: &str,
    prompt: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<crate::proc::Captured, crate::proc::ProcessError> {
    let env = crate::agent::agent_env();
    let args = [
        "--print",
        "--output-format",
        "json",
        "--model",
        model,
        "--allowed-tools",
        tools,
        "--max-budget-usd",
        budget_usd,
        "--permission-mode",
        "default",
        "--setting-sources",
        "project",
        "-p",
        prompt,
    ];
    crate::proc::capture_with_timeout_env(
        "claude",
        &args,
        cwd,
        crate::proc::Emit::quiet(),
        timeout,
        Some(&env),
    )
}

/// Refresh the notes for a package that has just built.
///
/// Errors are returned rather than panicking, so the caller can warn and let
/// the build stay successful.
/// What one notes turn did. The usage is kept even when the update fails: the
/// model call was paid for either way, and it counts toward the package.
pub struct NotesRun {
    pub usage: Usage,
    pub result: Result<(), String>,
}
