//! Generating the wheel specs: the multi-flavor and binary wheel specs and their
//! rpmlintrc and notes.

use super::*;

/// Wheels each locked to one CPython (Triton, onnxruntime): when EVERY flavor
/// the project builds has its own wheel for this arch, download them all and
/// write the per-flavor spec (convention 32 (section 49)) instead of leaving the agent to do
/// it by hand without being able to fetch the files. `None` when it does not
/// apply (fewer than two flavors, or a flavor with no wheel).
pub(super) fn generate_multi_flavor_wheel_spec(
    config: &PipelineConfig,
    json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    no_sdist: bool,
    guard: WheelGuard<'_>,
) -> Option<Result<(GenSpecResult, ap_ecosystem::pypi::WheelFile), GenSpecError>> {
    let flavors = match config.python_flavors.try_effective() {
        Ok(flavors) => flavors,
        Err(e) => return Some(Err(GenSpecError::Other(e))),
    };
    if flavors.len() < 2 {
        return None;
    }
    // Each flavor paired with its ABI tag. A flavor without one has no wheel to
    // pick, so the per-flavor layout does not apply.
    let mut flavor_abis: Vec<(String, String)> = Vec::with_capacity(flavors.len());
    for flavor in &flavors {
        let Some(abi) = crate::flavors::abi_tag_for_flavor(flavor) else {
            return None;
        };
        flavor_abis.push((flavor.clone(), abi));
    }
    let abis: Vec<String> = flavor_abis.iter().map(|(_, abi)| abi.clone()).collect();
    let picked = match ap_ecosystem::pypi::pick_wheels_per_flavor(json, &config.arch, &abis) {
        Ok(picked) => picked,
        Err(e) => {
            ap_core::diag!(
                "  cannot pick per-flavor wheels for {}: {e}",
                config.pkg_name
            );
            return None;
        }
    };
    // The wheels exist: the switch is judged before it writes anything.
    if let Err(why) = guard() {
        return Some(Err(GenSpecError::Refused(why)));
    }
    ap_core::diag!(
        "info: {} {}'s wheels are each locked to one CPython — repackaging one wheel per flavor ({})",
        config.pkg_name,
        config.version,
        flavors.join(", ")
    );
    Some((|| {
        if picked.len() != flavor_abis.len() {
            return Err(GenSpecError::Other(format!(
                "picked {} wheel(s) for {} flavor(s)",
                picked.len(),
                flavor_abis.len()
            )));
        }
        let mut wheels: Vec<(String, String)> = Vec::new();
        let mut first_path = None;
        for ((flavor, _), (_, wheel)) in flavor_abis.iter().zip(&picked) {
            let path = wheel
                .fetch(checkout_dir)
                .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
            first_path.get_or_insert(path);
            wheels.push((flavor.clone(), wheel.filename.clone()));
        }
        let Some((_, first_wheel)) = wheels.first().cloned() else {
            return Err(GenSpecError::Other(
                "no wheel was picked for any flavor".into(),
            ));
        };
        let release = ap_ecosystem::pypi::parse_release_with_tarball(json, &first_wheel)
            .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;
        let root_is_purelib = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_root_is_purelib);
        let layout = first_path
            .as_deref()
            .and_then(ap_ecosystem::sdist::wheel_layout);
        let spec_text =
            crate::spec::strip_ap_fix_comments(&crate::spec::wheel_multi_flavor_skeleton_shared(
                &config.pkg_name,
                &config.version,
                &wheels,
                &release,
                root_is_purelib,
                layout.as_ref(),
                &crate::spec::sibling_wheels(
                    &config.rpms_dir,
                    checkout_dir.parent(),
                    &config.pkg_name,
                ),
            ));
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        let spec_text = crate::spec::with_pip_ignore_requires_python(
            &spec_text,
            release.requires_python.as_deref(),
            &flavors,
        );
        let spec_text =
            with_wheel_spec_notes(config, &spec_text, &release, layout.as_ref(), no_sdist);
        // As for the single-wheel spec: a wheel without a licence text, switched
        // to from a sdist that has one, ships the sdist's as Source1.
        let spec_text = match layout
            .as_ref()
            .filter(|_| !no_sdist)
            .and_then(|l| crate::spec::copy_sdist_license(checkout_dir, &config.version, l))
        {
            Some(file) => crate::spec::with_license_source(&spec_text, &file),
            None => spec_text,
        };
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        write_wheel_rpmlintrc(
            checkout_dir,
            &config.pkg_name,
            &first_wheel,
            layout.as_ref(),
            run_dir(config).as_deref(),
        );
        let first_wheel = picked.into_iter().next().map(|(_, w)| w).ok_or_else(|| {
            GenSpecError::Other(format!(
                "no wheel picked for any flavor of {}",
                config.pkg_name
            ))
        })?;
        Ok((
            GenSpecResult {
                sdist_top_level: String::new(),
                is_maturin: false,
                is_rust: false,
                from_repo_archive: false,
            },
            first_wheel,
        ))
    })())
}

/// Write the wheel's `python-<name>.rpmlintrc` next to the spec when it is a
/// vendor-binary wheel (see [`crate::spec::wheel_rpmlintrc`]).
fn write_wheel_rpmlintrc(
    checkout_dir: &Path,
    pkg_name: &str,
    wheel_filename: &str,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    run: Option<&Path>,
) {
    match crate::spec::wheel_rpmlintrc(pkg_name, wheel_filename, layout) {
        Some((file, text)) => {
            write_or_warn(checkout_dir.join(&file), text);
            if let Some(run) = run {
                crate::osc::note_rpmlintrc_written(run, &file);
            }
            // An earlier release of the tool named it `python-<name>.rpmlintrc`.
            if let Err(why) = crate::spec::migrate_dot_rpmlintrc(checkout_dir, &file) {
                ap_core::diag!("  {why}");
            }
        }
        // A zero-byte rpmlintrc left by an earlier run is a Source99 that
        // does nothing: remove it.
        None => {
            for ext in ["-rpmlintrc", ".rpmlintrc"] {
                let stale = checkout_dir.join(format!("python-{}{ext}", pkg_name.to_lowercase()));
                if std::fs::metadata(&stale).is_ok_and(|m| m.len() == 0) {
                    let _ = std::fs::remove_file(stale);
                }
            }
        }
    }
}

/// The notes a repackaged-wheel spec carries whichever generator wrote it (one
/// wheel, or one per flavor): the no-sdist note for a true no-sdist release, and
/// for a prebuilt vendor library a note that it is not a Python package, with a
/// warning when its licence is not open source.
fn with_wheel_spec_notes(
    config: &PipelineConfig,
    spec_text: &str,
    release: &ap_ecosystem::pypi::Release,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    no_sdist: bool,
) -> String {
    // The note about a missing sdist is only true of a release with none.
    let spec_text = if no_sdist {
        crate::spec::with_no_sdist_note(spec_text)
    } else {
        spec_text.to_string()
    };
    // A wheel that is only a vendor's shared library is a C library carried
    // through PyPI, not a Python package. It is repackaged only because a
    // Python package loads it from site-packages; say so in the description,
    // and flag a licence that is not open source.
    let library_only = layout.is_some_and(|l| l.is_library_only());
    if library_only {
        let proprietary = crate::spec::license_is_proprietary(&release.license);
        ap_core::diag!(
            "warning: {} {} is a prebuilt vendor library, not a Python package{}",
            config.pkg_name,
            config.version,
            if proprietary {
                "; its licence is not open source, so this redistributes proprietary binaries"
            } else {
                ""
            }
        );
        crate::spec::note_prebuilt_library(&spec_text, proprietary)
    } else {
        spec_text
    }
}

pub(super) fn generate_wheel_binary_spec(
    config: &PipelineConfig,
    wheel: &ap_ecosystem::pypi::WheelFile,
    pypi_json: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    no_sdist: bool,
    guard: WheelGuard<'_>,
) -> Result<GenSpecResult, GenSpecError> {
    guard().map_err(GenSpecError::Refused)?;
    ap_core::diag!(
        "info: {} {} {} — repackaging the upstream wheel {}",
        config.pkg_name,
        config.version,
        if no_sdist {
            "has no sdist and no source repository"
        } else {
            "is switched to its wheel"
        },
        wheel.filename,
    );

    let wheel_path = wheel
        .fetch(checkout_dir)
        .map_err(|e| GenSpecError::Other(format!("wheel download failed: {e}")))?;
    let root_is_purelib = ap_ecosystem::sdist::wheel_root_is_purelib(&wheel_path);
    let layout = ap_ecosystem::sdist::wheel_layout(&wheel_path);

    let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &wheel.filename)
        .map_err(|e| GenSpecError::Other(format!("PyPI parse failed: {e}")))?;

    // Other wheels of the project already built into the same directories
    // (convention 76 (section 83)): co-owned, not listed whole.
    let siblings =
        crate::spec::sibling_wheels(&config.rpms_dir, checkout_dir.parent(), &config.pkg_name);
    let spec_text = crate::spec::wheel_binary_skeleton_shared(
        &config.pkg_name,
        &config.version,
        &wheel.filename,
        &release,
        root_is_purelib,
        layout.as_ref(),
        &siblings,
    );
    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
    // A wheel whose Requires-Python is capped below a built flavor needs pip told
    // so in both sections that run it, without waiting for a review turn.
    // An unreadable flavor setup is an error, not an empty list that would
    // leave the pip ignore out of the spec without a word.
    let flavors = config
        .python_flavors
        .try_effective()
        .map_err(GenSpecError::Other)?;
    let spec_text = crate::spec::with_pip_ignore_requires_python(
        &spec_text,
        release.requires_python.as_deref(),
        &flavors,
    );
    let spec_text = with_wheel_spec_notes(config, &spec_text, &release, layout.as_ref(), no_sdist);
    // A wheel without a licence text, switched to from a sdist that has one: the
    // sdist's is shipped as Source1 and owned as %license.
    let spec_text = match layout
        .as_ref()
        .filter(|_| !no_sdist)
        .and_then(|l| crate::spec::copy_sdist_license(checkout_dir, &config.version, l))
    {
        Some(file) => crate::spec::with_license_source(&spec_text, &file),
        None => spec_text,
    };
    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);
    write_wheel_rpmlintrc(
        checkout_dir,
        &config.pkg_name,
        &wheel.filename,
        layout.as_ref(),
        run_dir(config).as_deref(),
    );

    Ok(GenSpecResult {
        // A wheel unpacks flat — no wrapping top-level directory.
        sdist_top_level: String::new(),
        is_maturin: false,
        is_rust: false,
        from_repo_archive: false,
    })
}
