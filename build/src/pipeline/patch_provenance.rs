//! The upstream provenance of the carried patches in a run that has the switch
//! set (`lint`; plan 4.5). A patch whose upstream request is merged in the
//! packaged release is dropped, only with its removal proof; the header and the
//! spec comment of each upstream patch are written, and each verdict is a note
//! in the package notes. A patch is never a failure of the run.

use super::*;
use crate::patch_provenance::{self as pp, ForgeNet, LiveForge, Provenance, Verdict};
use crate::pipeline::removal_proof::drop_proof;

/// The stage: the forge is asked through [`LiveForge`]. Does nothing unless the
/// run set `patch_provenance`.
pub(super) fn provenance_stage(ctx: &RunCtx, _st: &mut RunState) -> Result<(), PipelineResult> {
    if !ctx.config.patch_provenance {
        return Ok(());
    }
    let hosts = crate::allowed_hosts::load_default();
    let forge = LiveForge {
        package: &ctx.config.rpm_name,
        hosts: &hosts,
    };
    provenance_stage_with(ctx, &forge);
    Ok(())
}

/// [`provenance_stage`] with the forge given (tests use a fake).
pub(super) fn provenance_stage_with(ctx: &RunCtx, net: &dyn ForgeNet) {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let files = patch_files(&spec, &ctx.checkout_dir);
    let patches: Vec<(String, String)> = files
        .iter()
        .filter_map(|(name, content)| {
            let text = String::from_utf8(content.clone()?).ok()?;
            Some((name.clone(), text))
        })
        .collect();
    if patches.is_empty() {
        return;
    }
    let changes = std::fs::read_to_string(
        ctx.checkout_dir
            .join(format!("{}.changes", ctx.config.rpm_name)),
    )
    .unwrap_or_default();
    let inputs = pp::Inputs {
        pkg: &ctx.config.rpm_name,
        spec: &spec,
        changes: &changes,
        version: &ctx.config.version,
        repo: crate::bugfix::upstream_repo_of_spec(&spec),
        patches: &patches,
    };
    let provenance = pp::assess(&inputs, net, crate::upstream_cache::refreshing());
    let mut spec_now = spec;
    let mut notes: Vec<String> = Vec::new();
    for p in &provenance {
        if let Some(dropped) = act_on(ctx, &mut spec_now, p, &mut notes) {
            if dropped {
                continue;
            }
        }
        let text = patches
            .iter()
            .find(|(n, _)| *n == p.name)
            .map(|(_, t)| t.as_str())
            .unwrap_or_default();
        write_header(ctx, &mut spec_now, p, text, &mut notes);
    }
    if !notes.is_empty() {
        append_notes(
            ctx.ws.notes(),
            &format!("\n## Patch provenance\n{}\n", notes.join("\n")),
        );
    }
}

/// What the verdict asks for. `Some(true)` when the patch was dropped; `Some(false)`
/// when it stays with a note; `None` when the verdict asks nothing here.
fn act_on(
    ctx: &RunCtx,
    spec: &mut String,
    p: &Provenance,
    notes: &mut Vec<String>,
) -> Option<bool> {
    let url = p
        .origin
        .as_ref()
        .map(|o| o.url.as_str())
        .unwrap_or_default();
    let name = p.name.as_str();
    match &p.verdict {
        Verdict::Redundant { tag } => Some(drop_merged(ctx, spec, name, url, tag, notes)),
        Verdict::ReleasedLater { tag, version } => {
            notes.push(format!(
                "- {name}: merged upstream in {tag}, after the packaged release: kept (a newer version takes it: {version})"
            ));
            Some(false)
        }
        Verdict::HeadMoved { .. } => {
            notes.push(format!(
                "- {name}: {url} moved with different code: not replaced in this run"
            ));
            Some(false)
        }
        Verdict::MovedNotComparable(why) => {
            notes.push(format!(
                "- {name}: {url} moved and cannot be compared: {why}"
            ));
            Some(false)
        }
        Verdict::Closed => {
            notes.push(format!(
                "- {name}: {url} is closed without a merge: kept (a maintainer decides)"
            ));
            Some(false)
        }
        Verdict::NotChecked(why) => {
            notes.push(format!("- {name}: upstream state not checked: {why}"));
            Some(false)
        }
        Verdict::Nothing => None,
    }
}

/// Drop a patch the packaged release has, when the removal proof holds. The
/// proof is the same one a removal needs; without it the patch stays.
fn drop_merged(
    ctx: &RunCtx,
    spec: &mut String,
    name: &str,
    url: &str,
    tag: &str,
    notes: &mut Vec<String>,
) -> bool {
    let files = patch_files(spec, &ctx.checkout_dir);
    let Some(proof) = drop_proof(&ctx.checkout_dir, &ctx.config.version, spec, &files, name) else {
        ctx.reporter.report(&Event::Warning {
            step: None,
            message: format!(
                "{}: {name} is merged upstream in {tag}, but the removal proof fails (merged in another form?): kept for a maintainer",
                ctx.rpm_name
            ),
        });
        notes.push(format!(
            "- {name}: merged upstream in {tag} ({url}); kept: the removal proof fails"
        ));
        return false;
    };
    let names = [name.to_string()];
    let without = without_patches(spec, &names);
    write_or_warn(&ctx.spec_path, &without);
    drop_series_lines(&ctx.checkout_dir, &names);
    let _ = std::fs::remove_file(ctx.checkout_dir.join(name));
    *spec = without;
    ctx.reporter.report(&Event::Warning {
        step: None,
        message: format!(
            "{}: removed {name}: merged upstream in {tag}, and the packaged release has it",
            ctx.rpm_name
        ),
    });
    warn_if_err(
        ctx.reporter,
        "adding the .changes entry for a patch",
        osc::add_release_changes_entry(
            &ctx.checkout_dir,
            &ctx.config.version,
            &crate::patch_check::proven_drop_bullet(name, proof),
            Emit::quiet(),
        ),
    );
    notes.push(format!(
        "- {name}: removed by the pipeline: merged upstream in {tag} ({url})"
    ));
    true
}

/// Write the header and the spec comment a patch is due, if any.
fn write_header(
    ctx: &RunCtx,
    spec: &mut String,
    p: &Provenance,
    text: &str,
    notes: &mut Vec<String>,
) {
    if let Some(new_text) = pp::header_for(p, text) {
        write_or_warn(ctx.checkout_dir.join(&p.name), new_text);
        notes.push(format!("- {}: upstream header written", p.name));
    }
    if let Some(new_spec) = pp::spec_comment_for(p, spec) {
        write_or_warn(&ctx.spec_path, &new_spec);
        *spec = new_spec;
    }
}
