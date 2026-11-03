//! Generating the specs of Rust and Python packages from a repository and the wheel
//! fallback.

use super::*;
use crate::pipeline::wheel_spec::*;

pub(super) fn generate_rust_spec(
    config: &PipelineConfig,
    github_url: &str,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = &config.pkg_name;
    let version = &config.version;
    let spec_text = format!(
        "%global rustflags '-Clink-arg=-Wl,-z,relro,-z,now'\n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        TODO: fill in summary\n\
         License:        TODO: fill in license\n\
         URL:            {github_url}\n\
         Source0:        %{{name}}-%{{version}}.tar.zst\n\
         Source1:        vendor.tar.zst\n\
         Source999:      README.suse-maint.md\n\
         BuildRequires:  cargo-packaging\n\
         BuildRequires:  pkgconf\n\
         BuildRequires:  pkgconfig\n\
         \n\
         %description\n\
         TODO: fill in description\n\
         \n\
         %prep\n\
         %autosetup -p1 -a1\n\
         \n\
         %build\n\
         %{{cargo_build}}\n\
         \n\
         %install\n\
         install -D -d -m 0755 %{{buildroot}}%{{_bindir}}\n\
         install -m 0755 %{{_builddir}}/%{{name}}-%{{version}}/target/release/{name} \\\n\
                 %{{buildroot}}%{{_bindir}}/{name}\n\
         \n\
         %files\n\
         %license LICENSE\n\
         %{{_bindir}}/{name}\n\
         \n\
         %changelog\n"
    );

    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);

    Ok(GenSpecResult {
        sdist_top_level: format!("{name}-{version}"),
        is_maturin: false,
        is_rust: true,
        from_repo_archive: true,
    })
}

/// Generate a Python spec from a repository archive rather than a PyPI sdist,
/// using py2pack for the spec body so macros, dependencies and license
/// handling match the normal PyPI-sourced flow.
///
/// `tarball_path` is an already-downloaded archive inside the checkout, and
/// `pypi_json` the project's PyPI metadata — empty when the project is not on
/// PyPI at all, in which case py2pack fills in what it can and the review turn
/// supplies the rest from the source.
pub(super) fn generate_repo_python_spec(
    config: &PipelineConfig,
    tarball_path: &Path,
    sdist_info: &ap_ecosystem::sdist::SdistInfo,
    pypi_json: &str,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = config.pkg_name.to_lowercase();
    let version = &config.version;

    let tarball_name = tarball_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{name}-{version}.tar.gz"));

    let sdist_top_level = sdist_info
        .top_level_dir
        .clone()
        .unwrap_or_else(|| format!("{name}-{version}"));

    // For monorepos, skip py2pack entirely — it only handles one project.
    // Emit a skeleton the review agent fills in; PyPI metadata is optional.
    if !sdist_info.subprojects.is_empty() {
        let release = ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &tarball_name).ok();
        let spec_text = crate::spec::monorepo_skeleton(
            &config.pkg_name,
            version,
            &sdist_info.subprojects,
            release.as_ref(),
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        return Ok(GenSpecResult {
            sdist_top_level,
            is_maturin: sdist_info.build_system.is_maturin(),
            is_rust: false,
            from_repo_archive: true,
        });
    }

    // Build a Release from the PyPI JSON metadata, substituting the forge
    // tarball name so py2pack writes the correct Source0.
    //
    // When pypi_json is empty, the project simply isn't on PyPI — this
    // function's own doc comment on `pypi_json` says that's expected
    // ("py2pack fills in what it can and the review turn supplies the
    // rest from the source"), and the design intent is to still produce
    // a usable skeleton. The code tries `parse_release_with_tarball("")`
    // first, gets `Error::Malformed`, and surfaces a hard
    // "project is not on PyPI, so py2pack has nothing to describe it
    // with" error that killed the whole pipeline run — exactly the
    // crash the user hit on `llama-cpp-scripts` (llama.cpp's root
    // pyproject stub that upstream never published). Synthesize a
    // minimal `Release` directly in that case so py2pack produces a
    // skeleton the Review agent can fill in from source.
    let release = if pypi_json.is_empty() {
        synthesize_release_without_pypi(name.clone(), version.clone(), tarball_name.clone())
    } else {
        ap_ecosystem::pypi::parse_release_with_tarball(pypi_json, &tarball_name).map_err(|e| {
            GenSpecError::Other(format!(
                "PyPI metadata for {name} {version} was returned but could not be \
                     parsed ({e}). This is distinct from the no-PyPI case (which uses \
                     the synthetic-release fallback) — a non-empty-but-malformed body \
                     usually means the index returned an error page where JSON was \
                     expected; inspect the request manually."
            ))
        })?
    };

    // Generate spec via py2pack (same path as a normal PyPI-sourced package).
    let spec_text = crate::spec::generate(&release, &sdist_info.build_system)
        .map_err(|e| GenSpecError::Other(format!("spec generation failed: {e}")))?;

    let spec_text = crate::spec::post_process_with_sdist(
        &spec_text,
        &release,
        &sdist_info.build_system,
        crate::spec::SdistFacts {
            has_tests: sdist_info.has_tests,
            has_c_extensions: sdist_info.has_c_extensions,
            top_level_tests_dir: sdist_info.top_level_tests_dir.clone(),
            console_scripts: sdist_info.console_scripts.clone(),
            install_entries: crate::spec::sdist_files_entries(
                &sdist_info.files,
                &sdist_top_level,
                sdist_info.project_dir.as_deref(),
            ),
        },
    );

    let spec_text =
        crate::spec::fix_autosetup_dir(&spec_text, &sdist_top_level).unwrap_or(spec_text);

    // A repository snapshot of a monorepo keeps the project one or more levels
    // down; %build and %install have to change into it.
    let spec_text = match &sdist_info.project_dir {
        Some(dir) => crate::spec::build_in_subdir(&spec_text, dir),
        None => spec_text,
    };

    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);

    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
    write_or_warn(step.file("spec.generated"), &spec_text);

    Ok(GenSpecResult {
        sdist_top_level,
        is_maturin: sdist_info.build_system.is_maturin(),
        is_rust: false,
        from_repo_archive: true,
    })
}

/// Convention 28 Exception 4: the agent decided THIS package's own source
/// cannot be compiled in this environment (e.g. it needs NVIDIA's `nvcc` to
/// build `.cu` device code, which OBS cannot provide) even though, unlike
/// [`generate_wheel_binary_spec`]'s callers, a real sdist exists. Re-fetches
/// the PyPI release JSON and, if it also publishes a wheel, overwrites
/// `spec_path` with the same wheel-binary skeleton the no-sdist path uses —
/// this is an independent check, not trust in the agent's claim: if PyPI has
/// no wheel either, the caller still fails the package.
///
/// Returns the picked [`ap_ecosystem::pypi::WheelFile`] so the caller can
/// write a provenance record alongside the spec. The only check is that a wheel
/// exists; the agent's claim that the source cannot be built is not verified
/// here. We cannot verify "source builds are
/// impossible" from here (that is a factual claim about an environment OBS
/// controls), but we can at least pin the exact artifact that got
/// substituted and the reasoning that justified it, so a human reviewer
/// has something to audit before the recipe is submitted.
pub(super) fn try_wheel_fallback(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<ap_ecosystem::pypi::WheelFile, String> {
    try_wheel_fallback_with(
        config,
        checkout_dir,
        spec_path,
        step,
        &fetch_release_json,
        &|| Ok(()),
    )
    .map_err(|e| e.reason)
}

/// How the wheel switch learns what PyPI has for a release: the JSON of
/// `https://pypi.org/pypi/<name>/<version>/json`. Injected so a test drives the
/// switch without the network.
pub(super) type PypiLookup<'a> = &'a dyn Fn(&PipelineConfig) -> Result<String, String>;

/// The release's PyPI JSON, from the network.
pub(super) fn fetch_release_json(config: &PipelineConfig) -> Result<String, String> {
    let pypi_url = pypi_json_url(&config.pkg_name, &config.version);
    crate::http::read_text(crate::http::get_with_retries(&pypi_url).map_err(|e| {
        format!(
            "PyPI fetch failed for {} {}: {e}",
            config.pkg_name, config.version
        )
    })?)
    .map_err(|e| {
        format!(
            "PyPI read failed for {} {}: {e}",
            config.pkg_name, config.version
        )
    })
}

pub(super) fn try_wheel_fallback_with(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    lookup: PypiLookup<'_>,
    guard: WheelGuard<'_>,
) -> Result<ap_ecosystem::pypi::WheelFile, TurnError> {
    let json = lookup(config)?;
    let wheel = match ap_ecosystem::pypi::pick_wheel_from_json(
        &json,
        &config.arch,
        config.single_locked_flavor_abi().as_deref(),
    ) {
        Ok(w) => w,
        Err(e) => {
            // No single wheel serves every flavor; one per flavor may (Triton).
            return match generate_multi_flavor_wheel_spec(
                config,
                &json,
                checkout_dir,
                spec_path,
                step,
                false,
                guard,
            ) {
                Some(Ok((_, first))) => Ok(first),
                Some(Err(GenSpecError::Refused(m))) => Err(TurnError::needs_maintainer(m)),
                Some(Err(
                    GenSpecError::Other(m)
                    | GenSpecError::NoReleaseArchive(m)
                    | GenSpecError::DiskFull(m),
                )) => Err(format!("per-flavor wheel spec generation failed: {m}").into()),
                Some(Err(GenSpecError::WheelOnly { .. })) | None => {
                    Err(format!("no usable upstream wheel either: {e}").into())
                }
            };
        }
    };
    generate_wheel_binary_spec(
        config,
        &wheel,
        &json,
        checkout_dir,
        spec_path,
        step,
        false,
        guard,
    )
    .map_err(|e| match e {
        GenSpecError::Refused(msg) => TurnError::needs_maintainer(msg),
        GenSpecError::Other(msg)
        | GenSpecError::NoReleaseArchive(msg)
        | GenSpecError::DiskFull(msg) => format!("wheel spec generation failed: {msg}").into(),
        GenSpecError::WheelOnly { .. } => "wheel spec generation failed unexpectedly"
            .to_string()
            .into(),
    })?;
    Ok(wheel)
}
