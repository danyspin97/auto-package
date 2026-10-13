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
/// Held as a `<rpm-name>.md.lock` file holding the owner's pid and start time,
/// created by linking a complete file into place (no locking crate is a
/// dependency). Removed on drop, including on the error path.
struct NotesLock {
    path: PathBuf,
}

/// Start time of `pid` (field 22 of `/proc/<pid>/stat`), so a lock whose pid
/// was reused by an unrelated process is not mistaken for a live one.
fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces or parens: count from the last `)`.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Whether the owner recorded in a lock file is still running. Without `/proc`
/// this cannot be told, so it answers yes rather than clobber a live build.
fn lock_text_is_held(text: &str) -> bool {
    let mut it = text.split_whitespace();
    let Some(pid) = it.next().and_then(|t| t.parse::<u32>().ok()) else {
        return false;
    };
    if pid == std::process::id() || !Path::new("/proc/self").exists() {
        return true;
    }
    match process_start(pid) {
        None => false,
        Some(now) => it
            .next()
            .and_then(|t| t.parse::<u64>().ok())
            .is_none_or(|recorded| recorded == now),
    }
}

impl NotesLock {
    fn acquire(notes_file: &Path) -> Result<Self, String> {
        Self::acquire_for(notes_file, LOCK_WAIT)
    }

    fn acquire_for(notes_file: &Path, wait: Duration) -> Result<Self, String> {
        let path = PathBuf::from(format!("{}.lock", notes_file.display()));
        let me = std::process::id();
        let start = process_start(me)
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        // Written in full before it becomes visible, so a second build never
        // reads a half-written lock and takes it for stale.
        let staging = PathBuf::from(format!("{}.{me}.tmp", path.display()));
        std::fs::write(&staging, format!("{me}{start}\n"))
            .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
        let deadline = std::time::Instant::now() + wait;
        let result = loop {
            match std::fs::hard_link(&staging, &path) {
                Ok(()) => break Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    if !lock_text_is_held(&text) {
                        // Stale. The rename is atomic, so of several builds
                        // clearing the same stale lock only one succeeds.
                        let moved = PathBuf::from(format!("{}.stale.{me}", path.display()));
                        if std::fs::rename(&path, &moved).is_ok() {
                            let _ = std::fs::remove_file(&moved);
                        }
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        break Err(format!(
                            "timed out waiting for {}: another build is updating these \
                             notes; delete the file if no build is running",
                            path.display()
                        ));
                    }
                    std::thread::sleep(LOCK_POLL.min(wait));
                }
                Err(e) => break Err(format!("cannot create {}: {e}", path.display())),
            }
        };
        let _ = std::fs::remove_file(&staging);
        result.map(|()| Self { path })
    }
}

impl Drop for NotesLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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

pub fn update_after_build(ws: &Workspace, rpm_name: &str, version: &str) -> NotesRun {
    let mut usage = Usage::default();
    let result = update_inner(ws, rpm_name, version, &mut usage);
    NotesRun { usage, result }
}

fn update_inner(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
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
    let before = std::fs::read_to_string(&notes_file).ok();
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
    let env = crate::agent::scrub_env_for_agent(std::env::vars().collect(), &[]);
    let out = crate::proc::capture_with_timeout_env(
        "claude",
        &[
            "--print",
            "--output-format",
            "json",
            "--model",
            NOTES_MODEL,
            "--allowed-tools",
            "Read,Write,Edit",
            "--max-budget-usd",
            NOTES_BUDGET_USD,
            "-p",
            &notes_prompt(rpm_name, version, &ws.notes(), scratch.path()),
        ],
        ws.dir(),
        crate::proc::Emit::quiet(),
        NOTES_TIMEOUT,
        Some(&env),
    )
    .map_err(|e| format!("could not run claude: {e}"))?;
    *usage = usage_from_output(&out.stdout);
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

fn notes_prompt(rpm_name: &str, version: &str, journal: &Path, notes_file: &Path) -> String {
    format!(
        "You are keeping the notes for the openSUSE package {rpm_name} \
         (version {version}), which just built successfully.\n\n\
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
        // This process is alive, so its own lock counts as held.
        let err = NotesLock::acquire_for(&notes, Duration::from_millis(300))
            .err()
            .expect("lock is held");
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(err.contains("p.md.lock"), "{err}");
        drop(first);
        assert!(!dir.path().join("p.md.lock").exists());
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
            Path::new("/w/notes.md"),
            Path::new("/n/python-foo.md"),
        );
        assert!(p.contains("python-foo"));
        assert!(p.contains("1.2.3"));
        assert!(p.contains("/w/notes.md"));
        assert!(p.contains("/n/python-foo.md"));
    }
}
