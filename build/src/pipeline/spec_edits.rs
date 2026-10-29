//! Spec and source edits: the stripped and rewritten spec, its restore, the
//! download service and the quilt series.

use super::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;

/// Drop stale `# ap: fix` comments from the spec on disk and return the result.
///
/// The returned text is the roll-back point for the edit the agent is about to
/// make, so the agent's own comment lines never accumulate across attempts.
pub(super) fn strip_and_rewrite(spec_path: &Path) -> String {
    let before = std::fs::read_to_string(spec_path).unwrap_or_else(|e| {
        ap_core::diag!("  cannot read {}: {e}", spec_path.display());
        String::new()
    });
    let clean = crate::spec::strip_ap_fix_comments(&before);
    if clean != before {
        write_or_warn(spec_path, &clean);
    }
    clean
}

/// Put `before` back as the spec after a rejected agent edit.
///
/// An empty `before` means the roll-back point could not be read (see
/// [`strip_and_rewrite`]); no real spec is empty, so writing it would
/// destroy the file the agent edited instead of restoring it. A failed
/// write is reported rather than dropped.
pub(super) fn restore_spec(
    spec_path: &Path,
    before: &str,
    step: Option<&StepDir>,
    reporter: &dyn Reporter,
) {
    let problem = if before.trim().is_empty() {
        Some("no readable roll-back point; leaving the spec as the agent left it".to_string())
    } else {
        ap_core::fsx::write_atomic(spec_path, before)
            .err()
            .map(|e| format!("writing the rolled-back spec failed: {e}"))
    };
    if let Some(message) = problem {
        reporter.report(&Event::Warning {
            step: step.map(|s| s.id),
            message: format!("{}: {message}", spec_path.display()),
        });
    }
}

/// A spec check that could not run. The edit is kept (the check did not judge
/// it), but the skip is warned about and journaled so it is not silent.
pub(super) fn skipped_check_note(
    reporter: &dyn Reporter,
    step: &StepDir,
    notes_path: PathBuf,
    check: &str,
    why: &str,
) {
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("the {check} check was skipped, so the edit is kept unchecked: {why}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## {check} check skipped\nThe check could not run, so the edit was kept without \
             it: {why}\n"
        ),
    );
}

/// Undo a turn that failed on the agent's side. The agent may have edited the
/// checkout or the spec before it failed, and nothing validates those edits,
/// so both go back to the state taken before the turn and the journal says so.
pub(super) fn discard_failed_turn(
    notes_path: PathBuf,
    reporter: &dyn Reporter,
    snapshot: &CheckoutSnapshot,
    spec_path: &Path,
    before: &str,
    label: &str,
) {
    snapshot.restore(reporter, None);
    restore_spec(spec_path, before, None, reporter);
    append_notes(
        notes_path,
        &format!("\n## {label} — the agent turn failed\nIts edits were discarded.\n"),
    );
}

/// Basenames of every `Source`/`Patch` file the spec references but that do
/// not exist in `checkout_dir`.
///
/// Takes `rpmspec --parse`'s output rather than the raw spec text so macros
/// (`%{name}-%{version}.tar.gz`) and conditionals are already resolved —
/// checking the raw spec would false-positive on any unexpanded macro. A URL
/// value (the common `Source:` shape for a plain PyPI tarball) is reduced to
/// its basename, the same name `osc::checkout`/`fetch_release_archive` save
/// it under locally.
pub(super) fn missing_source_files(expanded_spec: &str, checkout_dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for line in expanded_spec.lines() {
        let line = line.trim();
        let Some(tag) = ["Source", "Patch"].iter().find(|t| line.starts_with(**t)) else {
            continue;
        };
        let Some(rest) = line[tag.len()..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(':')
        else {
            continue;
        };
        let value = rest.trim();
        // Macro that failed to expand (shouldn't happen once `rpmspec --parse`
        // has already succeeded) — nothing reliable to check against disk.
        if value.is_empty() || value.contains('%') {
            continue;
        }
        let name = value.rsplit('/').next().unwrap_or(value);
        if !checkout_dir.join(name).exists() {
            missing.push(name.to_string());
        }
    }
    missing
}

/// The `_service` that fetches every `SourceN: <url>` of the spec: the
/// openSUSE `download_files` form, which reads the URLs (and `#/<name>` rename
/// fragments) from the spec itself.
const DOWNLOAD_FILES_SERVICE: &str = "<services>\n\
     \x20 <service name=\"download_files\" mode=\"manual\"/>\n\
     </services>\n";

/// The `http(s)://` URLs of the `SourceN:` lines of `expanded_spec` whose file
/// (the basename, or the `#/<name>` rename) is one of `missing`. A `Patch` line
/// and a value that is not an http(s) URL are never listed.
pub(super) fn missing_source_urls(expanded_spec: &str, missing: &[String]) -> Vec<String> {
    expanded_spec
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("Source")?;
            let value = rest
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .strip_prefix(':')?
                .trim();
            let is_url = ["https://", "http://"]
                .iter()
                .any(|p| value.len() > p.len() && value.starts_with(p));
            let name = value.rsplit('/').next().unwrap_or(value);
            (is_url && !value.contains(['%', ' ', '\t']) && missing.iter().any(|m| m == name))
                .then(|| value.to_string())
        })
        .collect()
}

/// Write a `download_files` `_service` when a missing Source is a URL and the
/// checkout has no `_service` yet; `true` when it was written. An agent that
/// adds `SourceN: <url>` cannot download it and often forgets the `_service`
/// that does (python-instanttensor, python-brotlicffi), which costs a turn.
/// An existing `_service` is the agent's or the pipeline's and is not touched.
pub(super) fn write_download_service(
    expanded_spec: &str,
    missing: &[String],
    checkout_dir: &Path,
) -> bool {
    let service = checkout_dir.join("_service");
    if missing.is_empty()
        || service.exists()
        || missing_source_urls(expanded_spec, missing).is_empty()
    {
        return false;
    }
    ap_core::fsx::write_atomic(&service, DOWNLOAD_FILES_SERVICE).is_ok()
}

/// Keep `checkout_dir/series` in sync with the spec's current `PatchN:`
/// lines. `QUILT_PATCHES` points `quilt new`/`quilt add`/`quilt refresh`
/// (see `agent::TurnCtx::checkout_dir`'s doc comment) at `checkout_dir`
/// directly, so a patch removed from the spec — by hand, by `--regen`, or
/// by an earlier agent turn — otherwise leaves its filename behind in
/// `series` forever: `%autosetup -p1`'s quilt backend chokes trying to
/// apply a patch the current spec no longer declares, which looks like a
/// spurious, unexplained `%prep` failure on a spec that presently declares
/// zero patches. Idempotent and run on
/// every `ValidateSpec` visit, like `missing_source_files`.
pub(super) fn sync_quilt_series(expanded_spec: &str, checkout_dir: &Path) {
    let mut patches = Vec::new();
    for line in expanded_spec.lines() {
        let line = line.trim();
        if !line.starts_with("Patch") {
            continue;
        }
        let Some(rest) = line["Patch".len()..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(':')
        else {
            continue;
        };
        let value = rest.trim();
        if value.is_empty() || value.contains('%') {
            continue;
        }
        patches.push(value.rsplit('/').next().unwrap_or(value).to_string());
    }

    let series_path = checkout_dir.join("series");
    let existing = std::fs::read_to_string(&series_path).ok();
    let wanted = match &existing {
        // A series that is there is edited, never rewritten: its order, flags
        // and comments are the maintainer's.
        Some(current) => minimal_series_update(current, &patches),
        None if patches.is_empty() => return,
        None => patches.join("\n") + "\n",
    };
    if existing.as_deref() == Some(wanted.as_str()) {
        return;
    }
    if wanted.trim().is_empty() {
        if let Err(e) = std::fs::remove_file(&series_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            ap_core::diag!("  cannot remove {}: {e}", series_path.display());
        }
        return;
    }
    write_or_warn(&series_path, &wanted);
}

/// `current` with only the lines that no longer match the spec removed (a
/// patch the spec stopped declaring) and the spec's new patches appended, in
/// spec order. Every other line keeps its place and text, so a series that is
/// already right comes back byte for byte.
fn minimal_series_update(current: &str, spec_patches: &[String]) -> String {
    let name_of = |line: &str| -> Option<String> {
        let t = line.trim();
        (!t.is_empty() && !t.starts_with('#'))
            .then(|| t.split_whitespace().next().unwrap_or(t).to_string())
    };
    let mut out: Vec<&str> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for line in current.lines() {
        match name_of(line) {
            Some(name) if !spec_patches.contains(&name) => {}
            Some(name) => {
                kept.push(name);
                out.push(line);
            }
            None => out.push(line),
        }
    }
    let mut text = out.join("\n");
    for p in spec_patches {
        if !kept.contains(p) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(p);
            text.push('\n');
        }
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if text == current.trim_end_matches('\n').to_string() + "\n" || text == current {
        return current.to_string();
    }
    text
}
