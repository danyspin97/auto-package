//! The proof a patch removal needs: a patch an edit removes is kept unless
//! quilt proves its change is already in the new sources.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::rejection::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// The patches a turn removed (its `PatchN:` line or its file) whose removal is
/// not proven: quilt, run on the new sources with the patches as they were
/// before the turn, does not report them `can be reverse-applied`, so their
/// change is not upstream. Empty when the turn removed nothing or every
/// removal is proven. `snapshot` holds the patch files as they were.
pub(super) fn unproven_patch_removals(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<String> {
    let before_files: std::collections::BTreeMap<String, Option<Vec<u8>>> =
        crate::patch_check::declared_patches(before)
            .into_iter()
            .filter(|(_, n)| !n.is_empty() && Path::new(n).file_name() == Some(n.as_ref()))
            .map(|(_, n)| {
                let content = snapshot
                    .and_then(|s| s.content_of(&n))
                    .or_else(|| std::fs::read(checkout_dir.join(&n)).ok());
                (n, content)
            })
            .collect();
    if before_files.is_empty() {
        return Vec::new();
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    let after_names: Vec<String> = crate::patch_check::declared_patches(&after)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let after_files = before_files
        .keys()
        .map(|n| (n.clone(), std::fs::read(checkout_dir.join(n)).ok()))
        .collect();
    let (_, removed) = crate::patch_check::diff_patches(&before_files, &after_names, &after_files);
    if removed.is_empty() {
        return Vec::new();
    }
    let tarball = find_tarball(checkout_dir, &config.version);
    let report = crate::patch_check::check_files(before, &before_files, tarball.as_deref());
    removed
        .into_iter()
        .filter(|n| !report.proves_applied(n))
        .collect()
}

/// What a turn is told when its removal of `names` was refused.
pub(super) fn unproven_removal_problem(names: &[String]) -> String {
    names
        .iter()
        .map(|n| {
            format!(
                "removal of {n} is not proven: quilt does not report it `can be \
                 reverse-applied` on the new sources, so its change is not in them; rebase it \
                 with quilt (`quilt push -f`, fix the rejected hunks, `quilt refresh`)"
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Roll back a turn whose edit removed patches without proof, and journal why.
pub(super) fn reject_unproven_removal(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    label: &str,
    unproven: &[String],
) -> String {
    let problem = unproven_removal_problem(unproven);
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("{label}: edit rejected: {problem}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## {label} — edit rejected: a patch removal is not proven\n{problem}\n\
             A patch is never removed because it does not apply. The edit was rolled back.\n"
        ),
    );
    problem
}
