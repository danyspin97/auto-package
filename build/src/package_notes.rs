//! Per-package notes, kept current after each successful build.
//!
//! One short Haiku turn reads the package's attempt journal (`notes.md`) and
//! rewrites `package-notes.md`: what the package needs to build and test, which
//! workarounds were applied and why, and what failed on the way. The journal
//! stays the agent's own writer; these notes are the durable summary for
//! people.

use std::time::Duration;

use crate::workspace::Workspace;

/// Haiku is enough for summarising a journal, and it is the cheapest tier.
const NOTES_MODEL: &str = "claude-haiku-5-5";
const NOTES_BUDGET_USD: &str = "0.20";
const NOTES_TIMEOUT: Duration = Duration::from_secs(300);

/// Refresh `package-notes.md` for a package that has just built.
///
/// Errors are returned rather than panicking, so the caller can warn and let
/// the build stay successful.
pub fn update_after_build(ws: &Workspace, rpm_name: &str, version: &str) -> Result<(), String> {
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
            &notes_prompt(rpm_name, version),
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
    if !ws.package_notes().exists() {
        return Err("the agent did not write package-notes.md".to_string());
    }
    Ok(())
}

fn notes_prompt(rpm_name: &str, version: &str) -> String {
    format!(
        "You are keeping the notes for the openSUSE package {rpm_name} \
         (version {version}), which just built successfully.\n\n\
         Read `notes.md` in the current directory: it is the attempt journal \
         from the build agent. Read `package-notes.md` too if it exists: it is \
         this package's memory from earlier builds. Then update `package-notes.md` \
         in place (create it if missing) as a short Markdown document with \
         these sections, omitting any that are empty. Keep what is still true, \
         correct what this build changed, and drop what no longer applies:\n\n\
         - **Build requirements**: BuildRequires and Requires that matter, and \
           why any non-obvious one is there.\n\
         - **Test changes**: tests deselected, ignored or patched, and why.\n\
         - **Workarounds**: patches, source fetches, environment fixes, and \
           the reason for each.\n\
         - **Failure history**: each failure that was fixed, one line each, \
           with the fix.\n\n\
         Keep it factual and under 60 lines. Do not copy the journal verbatim. \
         Do not touch any other file, and do not run commands."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_both_files_and_the_package() {
        let p = notes_prompt("python-foo", "1.2.3");
        assert!(p.contains("python-foo"));
        assert!(p.contains("1.2.3"));
        assert!(p.contains("`notes.md`"));
        assert!(p.contains("`package-notes.md`"));
    }
}
