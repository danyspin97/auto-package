//! Per-package notes, kept current after each successful build.
//!
//! One short Haiku turn reads the package's attempt journal (`notes.md`) and
//! updates the package's notes file: what the package needs to build and test,
//! which workarounds were applied and why, and what failed on the way. The
//! journal stays the agent's own writer. The notes live in a durable directory
//! (`notes/packages/` by default) rather than the work directory, so they
//! survive `ap-work` being cleaned up, and every later attempt for the package
//! reads them as memory.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use ap_core::event::Usage;

use crate::workspace::Workspace;

/// Haiku is enough for summarising a journal, and it is the cheapest tier.
const NOTES_MODEL: &str = "claude-haiku-5-5";
const NOTES_BUDGET_USD: &str = "0.20";
const NOTES_TIMEOUT: Duration = Duration::from_secs(300);

static NOTES_DIR: OnceLock<PathBuf> = OnceLock::new();

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

/// Write `contents` to `path` via a temporary file in the same directory and a
/// rename, so a concurrent build never sees (or leaves) half-written notes.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, contents.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// How long a build waits for another build's update of the same notes.
const LOCK_WAIT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Exclusive lock on one package's notes, so two builds cannot both read the
/// same notes and have the later writer silently discard the earlier update.
/// An `flock(2)` on `<rpm-name>.md.lock`, held while the file stays open; the
/// kernel releases it if the build dies, so a crash leaves nothing to clear.
/// The file itself stays behind: deleting a locked file would let a second
/// build lock a fresh one at the same path.
struct NotesLock {
    _file: std::fs::File,
}

impl NotesLock {
    fn acquire(notes_file: &Path) -> Result<Self, String> {
        Self::acquire_for(notes_file, LOCK_WAIT)
    }

    fn acquire_for(notes_file: &Path, wait: Duration) -> Result<Self, String> {
        use ap_core::proc_lock::{LockError, try_lock};
        let path = PathBuf::from(format!("{}.lock", notes_file.display()));
        let deadline = std::time::Instant::now() + wait;
        loop {
            match try_lock(&path) {
                Ok(file) => return Ok(Self { _file: file }),
                Err(LockError::Held(_)) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "timed out waiting for {}: another build is updating these notes",
                            path.display()
                        ));
                    }
                    std::thread::sleep(LOCK_POLL.min(wait));
                }
                Err(LockError::Io(e)) => {
                    return Err(format!("cannot create {}: {e}", path.display()));
                }
            }
        }
    }
}

/// One quiet `claude --print --output-format json` call with the scrubbed
/// agent environment, shared by the notes update and the source lookup.
pub fn claude_print_json(
    model: &str,
    tools: &str,
    budget_usd: &str,
    prompt: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<crate::proc::Captured, crate::proc::ProcessError> {
    let env = crate::agent::scrub_env_for_agent(std::env::vars().collect(), &[]);
    crate::proc::capture_with_timeout_env(
        "claude",
        &[
            "--print",
            "--output-format",
            "json",
            "--model",
            model,
            "--allowed-tools",
            tools,
            "--max-budget-usd",
            budget_usd,
            "-p",
            prompt,
        ],
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

pub fn update_after_build(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
) -> NotesRun {
    let mut usage = Usage::default();
    let result = update_inner(ws, rpm_name, version, project, distro, &mut usage);
    NotesRun { usage, result }
}

fn update_inner(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    usage: &mut Usage,
) -> Result<(), String> {
    let Some(notes_file) = path_for(rpm_name) else {
        return Ok(());
    };
    if let Some(dir) = notes_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Held across read, agent and write: a second build for the same package
    // waits here instead of overwriting this build's update.
    let _lock = NotesLock::acquire(&notes_file)?;
    // Missing is the normal first-build case; any other read error means the
    // existing notes would be overwritten from scratch, so stop instead.
    let before = match std::fs::read_to_string(&notes_file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", notes_file.display())),
    };
    // The agent edits a private copy and the result is renamed into place, so
    // a failed or concurrent build never leaves the shared notes half-written.
    let dir = notes_file.parent().unwrap_or_else(|| Path::new("."));
    // Not `.md`: a scratch file left behind by an interrupted run must not look
    // like a package's notes.
    let scratch = tempfile::Builder::new()
        .prefix(".scratch-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| {
            format!(
                "cannot create a scratch notes file in {}: {e}",
                dir.display()
            )
        })?;
    std::fs::write(scratch.path(), before.as_deref().unwrap_or_default())
        .map_err(|e| format!("cannot seed {}: {e}", scratch.path().display()))?;
    // The agent may only write its scratch file, and read the notes directory
    // (the journal is under the working directory, which is readable anyway).
    // `//` marks an absolute path in a permission rule.
    let abs = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        format!("/{}", p.display())
    };
    let tools = format!(
        "Read({dir}/**),Write({scratch}),Edit({scratch})",
        dir = abs(dir),
        scratch = abs(scratch.path()),
    );
    let out = claude_print_json(
        NOTES_MODEL,
        &tools,
        NOTES_BUDGET_USD,
        &notes_prompt(
            rpm_name,
            version,
            project,
            distro,
            &ws.notes(),
            scratch.path(),
        ),
        ws.dir(),
        NOTES_TIMEOUT,
    )
    .map_err(|e| match e {
        // Nothing ran, so nothing was spent.
        crate::proc::ProcessError::Launch { .. } => format!("could not run claude: {e}"),
        // The process ran and its reply, with the real cost, was lost. The
        // turn can have spent up to its cap, so charge that as an upper bound
        // rather than let a timed-out turn read as free.
        _ => {
            usage.cost_usd = NOTES_BUDGET_USD.parse().unwrap_or(0.0);
            format!(
                "could not run claude: {e}; cost counted as the ${NOTES_BUDGET_USD} \
                 turn cap, an estimate"
            )
        }
    })?;
    *usage = usage_from_output(&out.stdout);
    // A nonzero exit with no usage report (killed, or crashed before printing)
    // may still have spent money: charge the cap as an upper bound.
    if out.code != 0 && usage.cost_usd == 0.0 {
        usage.cost_usd = NOTES_BUDGET_USD.parse().unwrap_or(0.0);
    }
    if out.code != 0 {
        return Err(format!(
            "claude exited with {}: {}",
            out.code,
            out.stderr.trim()
        ));
    }
    // A notes file left by an earlier build is not an update from this one.
    let after = std::fs::read_to_string(scratch.path())
        .map_err(|e| format!("cannot read {}: {e}", scratch.path().display()))?;
    if after.trim().is_empty() || Some(&after) == before.as_ref() {
        return Err(format!("the agent did not update {}", notes_file.display()));
    }
    write_atomic(&notes_file, &after)
        .map_err(|e| format!("cannot write {}: {e}", notes_file.display()))?;
    Ok(())
}

/// Token counts and cost from `claude --output-format json`. A reply that does
/// not parse counts as free rather than failing the notes update.
fn usage_from_output(stdout: &str) -> Usage {
    let v: serde_json::Value = serde_json::from_str(stdout).unwrap_or_default();
    Usage {
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
    }
}

/// The package's notes as they stand, empty when there are none yet.
pub fn read(rpm_name: &str) -> String {
    path_for(rpm_name)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

fn notes_prompt(
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    journal: &Path,
    notes_file: &Path,
) -> String {
    format!(
        "You are keeping the notes for the openSUSE package {rpm_name} \
         (version {version}), which just built successfully in project \
         {project} for {distro}.\n\n\
         Read the attempt journal at `{journal}`: it is the build agent's own \
         record of this package. Read `{notes_file}` too if it exists: it is this \
         package's memory from earlier builds. Then update `{notes_file}` in place \
         (create it if missing) as a short Markdown document with these sections, \
         omitting any that are empty. Keep what is still true, correct what this \
         build changed, and drop what no longer applies:\n\n\
         - **Build requirements**: BuildRequires and Requires that matter, and \
           why any non-obvious one is there.\n\
         - **Test changes**: tests deselected, ignored or patched, and why.\n\
         - **Workarounds**: patches, source fetches, environment fixes, and \
           the reason for each.\n\
         - **Failure history**: each failure that was fixed, one line each, \
           with the fix.\n\n\
         The notes are shared by every project and distro this package is built \
         for. When a finding only applies to one target, name the project and \
         distro next to it (for example \"on openSUSE:Leap:16.0 only\"); leave \
         general findings unlabelled. Findings from earlier builds that name a \
         different target are not evidence about {project} / {distro}.\n\n\
         Keep it factual and under 60 lines. Do not copy the journal verbatim. \
         Do not touch any other file, and do not run commands.",
        journal = journal.display(),
        notes_file = notes_file.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_for_refuses_names_the_sanitiser_changes() {
        let dir = tempfile::tempdir().unwrap();
        set_notes_dir(dir.path().to_path_buf());
        assert!(path_for("../x").is_none());
        assert!(path_for("a/b").is_none());
        assert!(path_for("..").is_none());
    }

    #[test]
    fn a_second_acquire_waits_then_fails_naming_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("p.md");
        let first = NotesLock::acquire_for(&notes, Duration::from_millis(100)).unwrap();
        let started = std::time::Instant::now();
        // flock conflicts across open file descriptions, so this process's
        // own lock counts as held.
        let err = NotesLock::acquire_for(&notes, Duration::from_millis(300))
            .err()
            .expect("lock is held");
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(err.contains("p.md.lock"), "{err}");
        drop(first);
        NotesLock::acquire_for(&notes, Duration::from_millis(100)).unwrap();
    }

    #[test]
    fn atomic_write_replaces_notes_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("p.md");
        std::fs::write(&f, "old").unwrap();
        write_atomic(&f, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_write_keeps_previous_notes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("p.md");
        std::fs::write(&f, "old").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let res = write_atomic(&f, "new");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores directory permissions, so the write cannot be made to fail.
        if res.is_ok() {
            return;
        }
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old");
    }

    #[test]
    fn usage_is_read_from_the_cli_json_reply() {
        let u = usage_from_output(
            r#"{"result":"ok","total_cost_usd":0.0123,"usage":{"input_tokens":12,"output_tokens":34}}"#,
        );
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.output_tokens, 34);
        assert!((u.cost_usd - 0.0123).abs() < 1e-9);
    }

    #[test]
    fn unparseable_reply_is_free() {
        let u = usage_from_output("not json");
        assert_eq!(u.input_tokens + u.output_tokens, 0);
        assert_eq!(u.cost_usd, 0.0);
    }

    #[test]
    fn prompt_names_both_files_and_the_package() {
        let p = notes_prompt(
            "python-foo",
            "1.2.3",
            "home:me",
            "openSUSE_Leap_16.0",
            Path::new("/w/notes.md"),
            Path::new("/n/python-foo.md"),
        );
        assert!(p.contains("python-foo"));
        assert!(p.contains("1.2.3"));
        assert!(p.contains("home:me"));
        assert!(p.contains("openSUSE_Leap_16.0"));
        assert!(p.contains("/w/notes.md"));
        assert!(p.contains("/n/python-foo.md"));
    }
}
