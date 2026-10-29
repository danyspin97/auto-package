//! Switching a package to a wheel: the rung round, its reason and provenance.

use super::*;

/// The ladder round the mechanical wheel rung runs in: round 2, or two rounds
/// after a round that deferred to the npm vendoring route. The round in between
/// is an agent turn, so the npm route gets the turn it asked for before the
/// wheel switch is applied (the marker already in the notes would otherwise
/// turn the wheel switch through, with no npm attempt).
pub(super) fn wheel_rung_round(npm_deferred_round: Option<usize>) -> usize {
    npm_deferred_round.map_or(2, |r| r + 2)
}

pub(super) fn validate_wheel_switch_why(why: &str) -> Result<(), String> {
    let trimmed = why.trim();
    if trimmed.len() < 20 {
        return Err(format!(
            "use_upstream_wheel's `why` ({trimmed:?}) is too short to be \
             audit evidence — convention 28 Exception 4 asks for the exact \
             missing compiler/SDK and a quoted build.log line that proves it"
        ));
    }
    Ok(())
}

/// Record WHAT got swapped and WHY, so a human reviewing the OBS commit
/// after the pipeline has moved on can see the switch happened at all.
///
/// Writes `wheel-origin.txt` into `state/<pkg>/` and preserves the original
/// spec as `<spec>.pre-wheel-switch` before the fallback overwrites it.
/// Both files travel with the OBS commit and show up in the review diff,
/// instead of the switch being discoverable only from the run's JSONL
/// stream (which the OBS submit request's reviewers never see).
///
/// Non-fatal — a provenance-write failure surfaces as a warning rather
/// than reverting an otherwise-successful wheel switch; a wheel spec that
/// built the package correctly is still better than the source spec that
/// couldn't, with or without the audit file. The warning tells the
/// operator to add the file manually.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_wheel_switch_provenance(
    rpm_name: &str,
    config: &PipelineConfig,
    wheel: &ap_ecosystem::pypi::WheelFile,
    attempt: usize,
    label: &str,
    why: &str,
    original_spec: &str,
) -> std::io::Result<()> {
    let provenance = format!(
        "Switched from source build to upstream wheel.\n\
         \n\
         Package:      {rpm_name}\n\
         PyPI name:    {pkg_name}\n\
         Version:      {version}\n\
         Arch:         {arch}\n\
         Switch at:    {label}, attempt {attempt}\n\
         Pipeline:     auto-package convention 28 Exception 4\n\
         \n\
         Upstream wheel:\n\
         \x20 filename: {wheel_filename}\n\
         \x20 url:      {wheel_url}\n\
         \x20 sha256:   {wheel_sha256}\n\
         \x20 size:     {wheel_size} bytes\n\
         \n\
         Agent-reported reason (why the source cannot be compiled here):\n\
         {why}\n\
         \n\
         The pre-switch source-build spec is preserved as\n\
         `{rpm_name}.spec.pre-wheel-switch` in the package's state directory. Review both\n\
         before accepting the recipe; this file is the only record that\n\
         the recipe was swapped from source to binary.\n",
        pkg_name = config.pkg_name,
        version = config.version,
        arch = config.arch,
        wheel_filename = wheel.filename,
        wheel_url = wheel.url,
        wheel_sha256 = wheel.sha256,
        wheel_size = wheel.size,
    );
    // Kept in the package's state directory: files in the checkout are
    // "not committing" noise on every Commit step.
    // Through the workspace, which sanitizes the name: a raw `join` would let a
    // package name with `/` or `..` write outside the state root.
    let dir = Workspace::open(&config.state_root, rpm_name)?
        .dir()
        .to_path_buf();
    ap_core::fsx::write_atomic(&dir.join("wheel-origin.txt"), provenance)?;
    ap_core::fsx::write_atomic(
        &dir.join(format!(
            "{}.spec.pre-wheel-switch",
            ap_core::run::sanitize_name(rpm_name)
        )),
        original_spec,
    )?;
    Ok(())
}

/// Repackage a prebuilt wheel as-is, for a release with no sdist and no
/// discoverable source repository — a closed-source binary redistribution
/// (NVIDIA's individual CUDA toolkit component wheels are the running
/// example: nvcc, cupti, nvrtc, cudart have no public source at all). See
/// convention 28 Exception 3 in the agent prompt.
/// Repackage the release's upstream wheel when one is usable for every flavor;
/// otherwise report it wheel-only (with `candidates` for the operator).
pub(super) fn wheel_or_wheel_only(
    config: &PipelineConfig,
    json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    candidates: Vec<String>,
) -> Result<GenSpecResult, GenSpecError> {
    match ap_ecosystem::pypi::pick_wheel_from_json(
        json,
        &config.arch,
        config.single_locked_flavor_abi().as_deref(),
    ) {
        Ok(wheel) => {
            generate_wheel_binary_spec(config, &wheel, json, checkout_dir, spec_path, step, true)
        }
        Err(_) => {
            match generate_multi_flavor_wheel_spec(
                config,
                json,
                checkout_dir,
                spec_path,
                step,
                true,
            ) {
                Some(result) => result.map(|(r, _)| r),
                None => Err(GenSpecError::WheelOnly {
                    pkg_name: config.pkg_name.clone(),
                    version: config.version.clone(),
                    candidates,
                }),
            }
        }
    }
}
