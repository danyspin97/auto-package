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
/// The most one notes turn can cost. The turn runs under this `--max-budget-usd`,
/// so it is also the upper bound the caller reserves before the call.
pub const NOTES_CAP_USD: f64 = 0.20;
/// Largest notes file the turn may write. A bigger result is refused and the
/// previous notes stay; the prompt asks for under 60 lines.
const MAX_NOTES_LINES: usize = 60;
const MAX_NOTES_BYTES: usize = 8 * 1024;
const NOTES_TIMEOUT: Duration = Duration::from_secs(300);
/// Local git operations on the notes repository; they never touch a network.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

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

/// How long a build waits for another build's update of the same notes. The
/// holder keeps the lock for the whole agent turn and for the git calls that
/// commit the file, so the wait must outlast that or a healthy update reads as
/// a timeout.
const LOCK_WAIT: Duration =
    Duration::from_secs(NOTES_TIMEOUT.as_secs() + 5 * GIT_TIMEOUT.as_secs());

/// Exclusive lock on one package's notes, so two builds cannot both read the
/// same notes and have the later writer silently discard the earlier update.
/// An `flock(2)` on `<rpm-name>.md.lock`, held while the file stays open; the
/// kernel releases it if the build dies, so a crash leaves nothing to clear.
/// The lock file stays behind by design, one empty file per package: deleting a
/// locked file would let a second build lock a fresh one at the same path.
struct NotesLock {
    _file: std::fs::File,
}

impl NotesLock {
    fn acquire(notes_file: &Path) -> Result<Self, String> {
        Self::acquire_for(notes_file, LOCK_WAIT)
    }

    fn acquire_for(notes_file: &Path, wait: Duration) -> Result<Self, String> {
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
    let env = crate::agent::scrub_env_for_agent(std::env::vars().collect(), &[]);
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

pub fn update_after_build(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
) -> NotesRun {
    let mut usage = Usage::default();
    let result = update_inner(ws, rpm_name, version, project, distro, arch, &mut usage);
    NotesRun { usage, result }
}

fn update_inner(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
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
    let before = read_or_empty(&notes_file)
        .map_err(|e| format!("cannot read {}: {e}", notes_file.display()))?;
    // The agent edits a private copy and the result is renamed into place, so
    // a failed or concurrent build never leaves the shared notes half-written.
    let dir = notes_file.parent().unwrap_or_else(|| Path::new("."));
    // The lock is ours, so no live update for this package owns a scratch file.
    remove_stale_scratch(dir, rpm_name);
    // Not `.md`: a scratch file left behind by an interrupted run must not look
    // like a package's notes.
    let prefix = scratch_prefix(rpm_name);
    let scratch = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| {
            format!(
                "cannot create a scratch notes file in {}: {e}",
                dir.display()
            )
        })?;
    std::fs::write(scratch.path(), &before)
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
    // Path rules in `--allowed-tools` are only enforced in the default
    // permission mode with project-only settings; user settings could
    // otherwise widen what the agent may write.
    // The newest journal entries are inlined; older ones stay in the file.
    let journal_tail =
        crate::pipeline::tail_for_prompt(&std::fs::read_to_string(ws.notes()).unwrap_or_default());
    let out = claude_print_json(
        NOTES_MODEL,
        &tools,
        &format!("{NOTES_CAP_USD:.2}"),
        &notes_prompt(
            rpm_name,
            version,
            project,
            distro,
            arch,
            &ws.notes(),
            scratch.path(),
            &journal_tail,
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
            usage.cost_usd = NOTES_CAP_USD;
            format!(
                "could not run claude: {e}; cost counted as the ${NOTES_CAP_USD:.2} \
                 turn cap, an estimate"
            )
        }
    })?;
    *usage = usage_from_output(&out.stdout);
    // A nonzero exit with no usage report (killed, or crashed before printing)
    // may still have spent money: charge the cap as an upper bound.
    if out.code != 0 && usage.cost_usd == 0.0 {
        usage.cost_usd = NOTES_CAP_USD;
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
    if after.trim().is_empty() || after == before {
        return Err(format!("the agent did not update {}", notes_file.display()));
    }
    check_size(&after)?;
    ap_core::fsx::write_atomic(&notes_file, &after)
        .map_err(|e| format!("cannot write {}: {e}", notes_file.display()))?;
    // The notes are already saved; a failed commit must not fail the update.
    if let Err(e) = commit_notes(dir, &notes_file, rpm_name, version) {
        ap_core::diag!("  notes for {rpm_name} saved but not committed: {e}");
    }
    Ok(())
}

/// Prefix of one package's scratch files. `sanitize_name` never keeps a `~`, so
/// no package's prefix can match another package's files.
fn scratch_prefix(rpm_name: &str) -> String {
    format!(".scratch-{rpm_name}~")
}

/// Delete the scratch files an interrupted update left for `rpm_name`. Only
/// called while holding that package's lock.
fn remove_stale_scratch(dir: &Path, rpm_name: &str) {
    let prefix = scratch_prefix(rpm_name);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".tmp"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Refuse a notes file over the size cap, so one turn cannot grow the notes
/// without bound. The previous notes stay in place.
fn check_size(notes: &str) -> Result<(), String> {
    let lines = notes.lines().count();
    if lines > MAX_NOTES_LINES || notes.len() > MAX_NOTES_BYTES {
        return Err(format!(
            "the notes update is {lines} lines and {} bytes, over the \
             {MAX_NOTES_LINES}-line, {MAX_NOTES_BYTES}-byte cap; kept the previous notes",
            notes.len()
        ));
    }
    Ok(())
}

/// The commit message for an updated notes file.
fn commit_message(rpm_name: &str, version: &str) -> String {
    format!("notes: {rpm_name} {version}")
}

/// Commit `file` when `dir` is a git repository of its own, so the notes can
/// be shared and versioned as a git module. Never pushes. The repository is
/// the user's to create: without a `.git` entry (a directory, or the file a
/// submodule or worktree has) directly in `dir` this does nothing. A notes
/// directory that merely sits inside another repository, such as the project's
/// own checkout, is deliberately left alone rather than adding commits there.
///
/// Only `file` is committed (`commit -- <file>`), so if `git add` or `git
/// commit` fails, whatever else is staged in that repository is never swept
/// into a later notes commit; the file stays staged and the next update retries.
fn commit_notes(dir: &Path, file: &Path, rpm_name: &str, version: &str) -> Result<(), String> {
    if !dir.join(".git").exists() {
        return Ok(());
    }
    let git = |args: &[&str]| {
        let mut full: Vec<String> = vec!["-C".into(), dir.display().to_string()];
        full.extend(args.iter().map(|a| a.to_string()));
        crate::proc::capture_with_timeout_env(
            "git",
            &full,
            dir,
            crate::proc::Emit::quiet(),
            GIT_TIMEOUT,
            None,
        )
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
    };
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("notes file name is not UTF-8")?;
    let add = git(&["add", "--", name])?;
    if add.code != 0 {
        return Err(format!("git add failed: {}", add.stderr.trim()));
    }
    // Nothing to commit when the file already matches HEAD.
    let status = git(&["status", "--porcelain", "--", name])?;
    if status.code == 0 && status.stdout.trim().is_empty() {
        return Ok(());
    }
    let message = commit_message(rpm_name, version);
    // The environment's identity wins; the placeholder only keeps the commit
    // from failing where git has no identity configured.
    let has_identity = std::env::var_os("GIT_AUTHOR_EMAIL").is_some()
        || git(&["config", "user.email"]).is_ok_and(|o| !o.stdout.trim().is_empty());
    let mut args = Vec::new();
    if !has_identity {
        args.extend([
            "-c",
            "user.name=auto-package",
            "-c",
            "user.email=auto-package@localhost",
        ]);
    }
    args.extend(["commit", "-m", &message, "--", name]);
    let commit = git(&args)?;
    if commit.code != 0 {
        return Err(format!("git commit failed: {}", commit.stderr.trim()));
    }
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

/// Read a notes or journal file, treating a missing file as empty. Any other
/// error is returned: an unreadable file must not look like "no notes", or the
/// next update would overwrite it from scratch.
pub fn read_or_empty(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// The package's notes as they stand: `Ok("")` when there are none yet (or no
/// notes directory is set), an error when the file exists but cannot be read.
pub fn read_checked(rpm_name: &str) -> std::io::Result<String> {
    match path_for(rpm_name) {
        Some(p) => read_or_empty(&p),
        None => Ok(String::new()),
    }
}

/// [`read_checked`] for prompt building: a read error is reported on the
/// diagnostic stream and the prompt goes without notes, never silently.
pub fn read(rpm_name: &str) -> String {
    read_checked(rpm_name).unwrap_or_else(|e| {
        ap_core::diag!("  cannot read package notes for {rpm_name}: {e}");
        String::new()
    })
}

#[allow(clippy::too_many_arguments)]
fn notes_prompt(
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
    journal: &Path,
    notes_file: &Path,
    journal_tail: &str,
) -> String {
    format!(
        "You are keeping the notes for the openSUSE package {rpm_name} \
         (version {version}), which just built successfully in project \
         {project} for {distro}.\n\n\
         The newest part of the attempt journal is at the end of this prompt: \
         it is the build agent's own record of this package. Older entries are in \
         `{journal}` if you need them. Read `{notes_file}` too if it exists: it is \
         this package's memory from earlier builds. Then update `{notes_file}` in place \
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
         The notes are shared by every project, distro and architecture this \
         package is built for, so write each finding to hold on all of them. \
         Describe the rule, not one architecture's file: say \"the upstream wheel \
         for the build architecture\", not a wheel name or hash for x86_64. When a \
         finding only applies to one target, label it (for example \"on \
         openSUSE:Leap:16.0 only\" or \"on x86_64 only\"); leave general findings \
         unlabelled. This build is {project} / {distro} / {arch}; findings from \
         earlier builds that name a different target are not evidence about it.\n\n\
         Under each section you change, add a line saying which build it was \
         checked against, for example `Checked against {version} on {distro}, \
         {arch}.`, so a later build can judge how old the entry is.\n\n\
         Keep it factual and under 60 lines. Do not copy the journal verbatim. \
         Do not touch any other file, and do not run commands.\n\n\
         {journal_block}",
        journal = journal.display(),
        notes_file = notes_file.display(),
        journal_block = crate::agent::untrusted_data("journal", journal_tail),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_message_names_package_and_version() {
        assert_eq!(commit_message("python-foo", "1.2"), "notes: python-foo 1.2");
    }

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
    fn lock_wait_outlasts_a_healthy_holder() {
        // The holder runs the agent turn, then up to four git calls under the lock.
        assert!(LOCK_WAIT > NOTES_TIMEOUT + GIT_TIMEOUT * 4);
    }

    #[test]
    fn stale_scratch_is_removed_only_for_its_own_package() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join(".scratch-python-foo~abc123.tmp");
        let other = dir.path().join(".scratch-python-foo-bar~abc123.tmp");
        let notes = dir.path().join("python-foo.md");
        for p in [&mine, &other, &notes] {
            std::fs::write(p, "x").unwrap();
        }
        remove_stale_scratch(dir.path(), "python-foo");
        assert!(!mine.exists());
        assert!(other.exists());
        assert!(notes.exists());
    }

    #[test]
    fn atomic_write_replaces_notes_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("p.md");
        std::fs::write(&f, "old").unwrap();
        ap_core::fsx::write_atomic(&f, "new").unwrap();
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
        let res = ap_core::fsx::write_atomic(&f, "new");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores directory permissions, so the write cannot be made to fail.
        if res.is_ok() {
            return;
        }
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old");
    }

    #[test]
    fn missing_file_reads_as_empty_but_other_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_or_empty(&dir.path().join("none.md")).unwrap(), "");
        let f = dir.path().join("p.md");
        std::fs::write(&f, "text").unwrap();
        assert_eq!(read_or_empty(&f).unwrap(), "text");
        // A directory where a file is expected fails with a non-NotFound error.
        assert!(read_or_empty(dir.path()).is_err());
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
    fn a_notes_file_over_the_cap_is_refused() {
        assert!(check_size("## Workarounds\n- one\n").is_ok());
        let long: String = (0..MAX_NOTES_LINES + 1)
            .map(|i| format!("- {i}\n"))
            .collect();
        let err = check_size(&long).unwrap_err();
        assert!(err.contains("kept the previous notes"), "{err}");
        assert!(check_size(&"x".repeat(MAX_NOTES_BYTES + 1)).is_err());
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
            "x86_64",
            Path::new("/w/notes.md"),
            Path::new("/n/python-foo.md"),
            "the newest entry",
        );
        assert!(p.contains("the newest entry"));
        assert!(p.contains("python-foo"));
        assert!(p.contains("1.2.3"));
        assert!(p.contains("home:me"));
        assert!(p.contains("openSUSE_Leap_16.0"));
        assert!(p.contains("/w/notes.md"));
        assert!(p.contains("/n/python-foo.md"));
    }
}
