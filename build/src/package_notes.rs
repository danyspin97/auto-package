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
pub fn path_for(rpm_name: &str) -> Option<PathBuf> {
    NOTES_DIR
        .get()
        .map(|dir| dir.join(format!("{rpm_name}.md")))
}

/// Refresh the notes for a package that has just built.
///
/// Errors are returned rather than panicking, so the caller can warn and let
/// the build stay successful.
pub fn update_after_build(ws: &Workspace, rpm_name: &str, version: &str) -> Result<(), String> {
    let Some(notes_file) = path_for(rpm_name) else {
        return Ok(());
    };
    if let Some(dir) = notes_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
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
            &notes_prompt(rpm_name, version, &ws.notes(), &notes_file),
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
    if !notes_file.exists() {
        return Err(format!("the agent did not write {}", notes_file.display()));
    }
    Ok(())
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
