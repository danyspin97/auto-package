//! Build logs: extracting the failure from a log, linking it, and the notes on
//! dropped tests.

use super::*;
use crate::pipeline::licence::*;

/// Largest build log the test-dropping rung reads, from its end.
const DROP_LOG_CAP: u64 = 8 * 1024 * 1024;

/// The `.changes` line for tests the pipeline dropped from `%check`.
pub(super) fn dropped_tests_message(dropped: &crate::deselect::Dropped) -> String {
    let ids: Vec<&str> = dropped.tests.iter().map(|t| t.id.as_str()).collect();
    changes_message(&format!("Skip the failing tests {}", ids.join(", ")))
}

/// The spec edit of the test-dropping rung: the failing tests of a `%check`
/// log skipped with their log lines as evidence. Only when the failure was in
/// `%check` (so the build and the install had succeeded) and is a test failure,
/// not a dependency one. `Err` is why nothing was dropped.
pub(super) fn dropped_tests_edit(
    failed_section: Option<&str>,
    log: &str,
    spec: &str,
    source_has: &dyn Fn(&str) -> bool,
) -> Result<crate::deselect::Dropped, String> {
    if failed_section != Some("%check") {
        return Err(format!(
            "the build failed in {}, not in %check",
            failed_section.unwrap_or("an unknown phase")
        ));
    }
    crate::deselect::drop_failing_tests(spec, log, source_has)
}

/// The tail of a build log, read up to [`DROP_LOG_CAP`] bytes.
pub(super) fn read_log_tail(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > DROP_LOG_CAP {
        f.seek(SeekFrom::Start(len - DROP_LOG_CAP)).ok()?;
    }
    let mut bytes = Vec::new();
    f.take(DROP_LOG_CAP).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Point the durable workspace's `build.log` at this run's build log.
///
/// Uses a hard link rather than a symlink so the workspace entry stays a
/// plain file for the agent. The claude CLI's `--add-dir` sandbox resolves
/// a symlink to its real target before admitting the read, and the real
/// target lives under `runs/<run-id>/<pkg>/05-build/` — a sibling of the
/// workspace, outside anything the agent is granted. A symlink there surfaces
/// to the agent as `"the symlink target is outside the allowed working
/// directories"` and blocks the entire turn on an otherwise readable file.
/// A hard link appears to the sandbox as the workspace path itself and is
/// admitted; `fetch_buildlog`'s `std::fs::write(log_path, ...)` truncates
/// the inode in place, so both entries continue to reflect the latest log.
///
/// `hard_link` fails with `EXDEV` across filesystems; the copy fallback
/// covers that at the cost of duplicating the bytes.
pub(super) fn link_build_log(ws: &Workspace, build_log: &Path) {
    let link = ws.build_log();
    // Link under a temp name and rename over `build.log`, so the agent never
    // finds the workspace without a log between the remove and the create.
    let tmp = link.with_extension(format!("log.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let linked = std::fs::hard_link(build_log, &tmp)
        .or_else(|_| std::fs::copy(build_log, &tmp).map(|_| ()))
        .and_then(|()| std::fs::rename(&tmp, &link));
    if let Err(e) = linked {
        let _ = std::fs::remove_file(&tmp);
        ap_core::diag!(
            "  cannot link {} to {}: {e}",
            link.display(),
            build_log.display()
        );
    }
}

/// Run the extractor over a build log, warning when the sdist root never matched.
///
/// The whole log is handed to [`crate::log::extract`] — it segments the phases
/// itself and knows that a post-build check failure lives *after* `%check`, so
/// pre-scoping the text here would cut the failure out of some logs.
pub(super) fn extract_build_log(
    build_log: &Path,
    sdist_top_level: &str,
    reporter: &dyn Reporter,
    step: Option<ap_core::event::StepId>,
) -> crate::log::ExtractedLog {
    let log_text = std::fs::read_to_string(build_log).unwrap_or_default();
    let mapper = crate::log::PathMapper::with_sdist_root(sdist_top_level);
    let extracted = crate::log::extract(&log_text, &mapper);

    if !mapper.did_match_root() && !log_text.is_empty() {
        reporter.report(&Event::Progress {
            step,
            message: format!(
                "no path in the build log sits under the sdist root '{sdist_top_level}'; \
                 source files cannot be located and traceback frames may be over-elided"
            ),
        });
    }
    // Missing osc timestamp prefixes are not warned about: the extractor still
    // works, with slightly less accurate anchoring, and there is nothing to act on.

    extracted
}

/// Analyse a failed build log inside its own `ExtractFailure` step.
///
/// A step directory that cannot be created only loses the step's report, not
/// the analysis the agent needs, so it falls back to extracting unreported.
pub(super) fn extract_failure_step(
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
    step_attempts: &mut Attempts,
    build_log: &Path,
    sdist_top_level: &str,
    build_step: ap_core::event::StepId,
) -> crate::log::ExtractedLog {
    match StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::ExtractFailure,
        None,
        step_attempts,
    ) {
        Ok(step) => {
            let extract = extract_build_log(build_log, sdist_top_level, reporter, Some(step.id));
            step.finish(StepOutcome::Ok);
            extract
        }
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(build_step),
                message: format!("could not report the failure analysis step: {e}"),
            });
            extract_build_log(build_log, sdist_top_level, reporter, Some(build_step))
        }
    }
}
