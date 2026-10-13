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

/// Refresh the notes for a package that has just built.
///
/// Errors are returned rather than panicking, so the caller can warn and let
/// the build stay successful.
pub fn update_after_build(ws: &Workspace, rpm_name: &str, version: &str) -> Result<Usage, String> {
    let Some(notes_file) = path_for(rpm_name) else {
        return Ok(Usage::default());
    };
    if let Some(dir) = notes_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
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
    Ok(usage_from_output(&out.stdout))
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
