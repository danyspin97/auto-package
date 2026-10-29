//! Generating a spec: the entry points, the errors, and the wheel policy.

use super::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::python_spec::*;

pub(super) enum GenSpecError {
    WheelOnly {
        pkg_name: String,
        version: String,
        candidates: Vec<String>,
    },
    /// The repository exists but has no release archive for the wanted
    /// version (a guessed repo for a wheel-only vendor package, or a tag
    /// scheme the archive-URL guesses don't cover). Carries the full message.
    NoReleaseArchive(String),
    /// The disk is full while the spec is written or the sdist is read or
    /// fetched. Typed where the error was, so it does not depend on its wording.
    DiskFull(String),
    Other(String),
}

/// A generation failure from a step that writes or reads the disk: a full
/// disk is [`GenSpecError::DiskFull`], anything else [`GenSpecError::Other`].
fn disk_gen_error(message: String, disk_full: bool) -> GenSpecError {
    if disk_full {
        GenSpecError::DiskFull(message)
    } else {
        GenSpecError::Other(message)
    }
}

impl From<String> for GenSpecError {
    fn from(s: String) -> Self {
        GenSpecError::Other(s)
    }
}

pub(super) struct GenSpecResult {
    pub(super) sdist_top_level: String,
    pub(super) is_maturin: bool,
    /// A standalone Rust crate, built with cargo-packaging rather than the
    /// Python macros.
    pub(super) is_rust: bool,
    /// The source came from a repository archive rather than a PyPI sdist, so
    /// it has a repository's shape and not an sdist's (agent convention 22).
    pub(super) from_repo_archive: bool,
}

/// Whether `--source-url <pkg>=wheel` asks for a wheel repackage and the checked
/// out spec still builds from source: such a package is not done, whatever OBS
/// shows for it.
pub(super) fn wheel_policy_pending(
    source_urls: &crate::sources::SourceMap,
    pkg_name: &str,
    spec_path: &Path,
) -> bool {
    source_urls.get(pkg_name) == Some(WHEEL_SOURCE)
        && std::fs::read_to_string(spec_path).is_ok_and(|text| {
            !text
                .lines()
                .any(|l| l.trim_start().starts_with("Source") && l.contains(".whl"))
        })
}

/// Spec for a CUDA component (`--nvidia-redist`): NVIDIA's archive is
/// downloaded into the checkout, verified against the manifest and listed
/// without unpacking, and the spec and its rpmlintrc are made from the list.
/// Nothing of the Python flow applies: no sdist, no `%pyproject`, no `%check`.
pub(super) fn generate_redist_spec(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    artifact: Option<&Path>,
    source: &dyn crate::redist::Source,
) -> Result<GenSpecResult, GenSpecError> {
    ap_core::diag!(
        "info: {} {} is a CUDA component — packaging NVIDIA's redistributable archive",
        config.rpm_name,
        config.version,
    );
    let generated = crate::redist::generate(
        source,
        &config.rpm_name,
        &config.version,
        &config.arch,
        checkout_dir,
    )
    .map_err(GenSpecError::Other)?;
    let spec = crate::spec::strip_ap_fix_comments(&generated.spec);
    ap_core::fsx::write_atomic(spec_path, &spec).map_err(|e| {
        disk_gen_error(
            format!("writing spec failed: {e}"),
            ap_core::fsx::is_disk_full(&e),
        )
    })?;
    if let Some(artifact) = artifact {
        write_or_warn(artifact, &spec);
    }
    write_or_warn(
        checkout_dir.join(&generated.rpmlintrc_file),
        &generated.rpmlintrc,
    );
    // The source of an earlier release goes with the commit.
    crate::redist::remove_stale_archives(checkout_dir, &generated.archive);
    Ok(GenSpecResult {
        // The source is not unpacked: there is no tree to review.
        sdist_top_level: String::new(),
        is_maturin: false,
        is_rust: false,
        from_repo_archive: false,
    })
}

pub(super) fn generate_spec(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    // `--nvidia-redist`: a CUDA component comes from NVIDIA's own archive.
    if crate::redist::is_redist_package(&config.rpm_name) {
        return generate_redist_spec(
            config,
            checkout_dir,
            spec_path,
            Some(&step.file("spec.generated")),
            &crate::redist::Network,
        );
    }
    // `--source-url <package>=wheel`: the upstream wheel, whatever else exists.
    if config.prefers_wheel() {
        let json = release_json_for(config)?;
        let result = wheel_or_wheel_only(config, &json, checkout_dir, spec_path, step, Vec::new());
        if result.is_ok() {
            purge_source_recipe(checkout_dir);
        }
        return result;
    }
    // An explicit repository bypasses PyPI entirely: there may be no PyPI
    // project at all, and if there is, the user asked for the repo anyway.
    if let Some(ref repo_url) = config.source_repo {
        match generate_from_repo(config, repo_url, checkout_dir, spec_path, step) {
            // A repository with no archive for this version cannot be the
            // source of this release (a guessed repo for a wheel-only vendor
            // package, or an unusual tag scheme): use PyPI's own artifacts
            // below rather than failing the package outright.
            Err(GenSpecError::NoReleaseArchive(msg)) => {
                ap_core::diag!("  {msg}\n  falling back to PyPI's own release artifacts");
            }
            result => return result,
        }
    }

    use ap_ecosystem::pypi;
    use ap_ecosystem::sdist;

    let json = release_json_for(config)?;
    let release = match pypi::parse_release(&json) {
        Ok(r) => r,
        Err(pypi::Error::NoSdist { .. }) => {
            // Wheel-only on PyPI: build from a source repository instead.
            // An explicit --source-url wins over whatever the metadata says —
            // the user looked it up, we guessed.
            let source_url = config
                .source_urls
                .get(&config.pkg_name)
                .map(str::to_string)
                .or_else(|| pypi::source_url_from_json(&json));

            let source_url = match source_url {
                Some(url) => url,
                None => {
                    // Auto-discover a source repository. Only a *verified*
                    // (confirmed to exist) candidate is safe to build from
                    // automatically — an unverified guess just burns an
                    // attempt on a 404. See `RepoCandidates`.
                    let repos = find_source_repo_candidates(&config.pkg_name);
                    match repos.verified.first() {
                        Some(url) => {
                            ap_core::diag!(
                                "info: auto-discovered and verified source repository for {}: {url}",
                                config.pkg_name,
                            );
                            url.clone()
                        }
                        None => {
                            // No source anywhere. This is the normal shape of a
                            // closed-source binary redistribution (NVIDIA's
                            // individual CUDA toolkit component wheels are the
                            // running example) — repackage the wheel itself
                            // rather than getting permanently stuck. See
                            // convention 28 Exception 3.
                            return wheel_or_wheel_only(
                                config,
                                &json,
                                checkout_dir,
                                spec_path,
                                step,
                                repos.guessed,
                            );
                        }
                    }
                }
            };

            let stem = checkout_dir.join(format!(
                "{}-{}",
                safe_file_part(&config.pkg_name.to_lowercase()),
                safe_file_part(&config.version)
            ));
            let (url, tarball) = match fetch_release_archive(&source_url, &config.version, &stem) {
                Ok(found) => found,
                // The repository exists but has no archive for THIS version (a
                // guessed repo for a vendor wheel such as nvidia-nccl-cu13, or an
                // unusual tag scheme): it cannot be this release's source. Use
                // PyPI's own artifact instead of failing the package.
                Err(GenSpecError::NoReleaseArchive(msg)) => {
                    ap_core::diag!("  {msg}\n  falling back to PyPI's wheel");
                    return wheel_or_wheel_only(
                        config,
                        &json,
                        checkout_dir,
                        spec_path,
                        step,
                        vec![source_url.clone()],
                    );
                }
                Err(e) => return Err(e),
            };
            let digest = sha256_file(&tarball).map_err(|e| {
                disk_gen_error(
                    format!("hashing {}: {e}", tarball.display()),
                    ap_core::fsx::is_disk_full(&e),
                )
            })?;
            let info = ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
                .map_err(|e| disk_gen_error(format!("inspecting {url}: {e}"), e.is_disk_full()))?;

            let result =
                generate_repo_python_spec(config, &tarball, &info, &json, spec_path, step)?;
            record_archive_digest(spec_path, &url, &digest);
            return Ok(result);
        }
        Err(e) => return Err(GenSpecError::Other(format!("PyPI parse failed: {e}"))),
    };

    // Download tarball to checkout dir.
    let tarball = release.sdist.fetch(checkout_dir).map_err(|e| {
        disk_gen_error(
            format!("tarball download failed for {}: {e}", release.sdist.url),
            e.is_disk_full(),
        )
    })?;

    // Inspect sdist for build system info and top-level dir.
    let sdist_info = sdist::inspect(&tarball)
        .map_err(|e| disk_gen_error(format!("sdist inspect failed: {e}"), e.is_disk_full()))?;

    let is_maturin = sdist_info.build_system.is_maturin();

    // For monorepos, skip py2pack (it only handles one project) and emit a
    // skeleton the review agent will fill in with all subpackages.
    if !sdist_info.subprojects.is_empty() {
        let spec_text = crate::spec::monorepo_skeleton(
            &config.pkg_name,
            &config.version,
            &sdist_info.subprojects,
            Some(&release),
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        write_or_warn(step.file("spec.generated"), &spec_text);
        let sdist_top_level = sdist_info
            .top_level_dir
            .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));
        return Ok(GenSpecResult {
            sdist_top_level,
            is_maturin,
            is_rust: false,
            from_repo_archive: false,
        });
    }

    // Generate spec via py2pack.
    let spec_text = crate::spec::generate(&release, &sdist_info.build_system)
        .map_err(|e| GenSpecError::Other(format!("spec generation failed: {e}")))?;

    // Post-process with test info.
    let spec_text = crate::spec::post_process_with_sdist(
        &spec_text,
        &release,
        &sdist_info.build_system,
        crate::spec::SdistFacts {
            has_tests: sdist_info.has_tests,
            has_c_extensions: sdist_info.has_c_extensions,
            top_level_tests_dir: sdist_info.top_level_tests_dir.clone(),
            console_scripts: sdist_info.console_scripts.clone(),
            install_entries: sdist_info.top_level_dir.as_deref().and_then(|top| {
                crate::spec::sdist_files_entries(
                    &sdist_info.files,
                    top,
                    sdist_info.project_dir.as_deref(),
                )
            }),
        },
    );

    // Fix autosetup -n if needed.
    let spec_text = if let Some(top) = &sdist_info.top_level_dir {
        crate::spec::fix_autosetup_dir(&spec_text, top).unwrap_or(spec_text)
    } else {
        spec_text
    };

    // Strip any stray ap:fix comments.
    let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);

    // Write to checkout dir.
    ap_core::fsx::write_atomic(spec_path, &spec_text)
        .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;

    // Save artifact copy.
    write_or_warn(step.file("spec.generated"), &spec_text);

    let sdist_top_level = sdist_info
        .top_level_dir
        .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));

    Ok(GenSpecResult {
        sdist_top_level,
        is_maturin,
        is_rust: false,
        from_repo_archive: false,
    })
}

/// Build from a source repository the user pointed at.
///
/// Which generator to use is decided by what the release archive contains, not
/// by which flag was passed: a repository is not Rust just because it was
/// given as a URL, and a maturin project has both a `Cargo.toml` and a
/// `pyproject.toml`, so Python wins whenever Python packaging is present.
fn generate_from_repo(
    config: &PipelineConfig,
    repo_url: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
) -> Result<GenSpecResult, GenSpecError> {
    let name = config.pkg_name.to_lowercase();
    let stem = checkout_dir.join(format!(
        "{}-{}",
        safe_file_part(&name),
        safe_file_part(&config.version)
    ));

    let (url, tarball) = fetch_release_archive(repo_url, &config.version, &stem)?;
    let digest = sha256_file(&tarball)
        .map_err(|e| GenSpecError::Other(format!("hashing {}: {e}", tarball.display())))?;

    // Same preference the probe used, so generation and naming agree on which
    // subproject of a monorepo is being built.
    let info = ap_ecosystem::sdist::inspect_preferring(&tarball, Some(&config.pkg_name))
        .map_err(|e| GenSpecError::Other(format!("inspecting {url}: {e}")))?;

    // Mirror `sources::probe`: a CMake/autotools root with a non-matching
    // pyproject.toml beside it is a C/C++ project with a Python-helper
    // sidecar (llama.cpp's root `pyproject` declares `llama-cpp-scripts`
    // for shipping helper scripts), not a Python project. Without this
    // check `info.python_packaging` wins the ecosystem decision below
    // and the pipeline either falls into the mismatch-block's
    // `try_wheel_fallback` (no PyPI wheel exists for a C++ library like
    // llama.cpp, so it dies with "no usable upstream wheel either") or
    // — when the pyproject name happens to match the wanted name —
    // calls py2pack on a C/C++ project. Probe already decided PlainC
    // via `is_python_hijack`; align the generator with that decision.
    let is_python_hijack = crate::sources::is_python_hijack(&info, &config.pkg_name);

    // A monorepo may hold several projects, and `inspect_preferring` falls
    // back to the shallowest manifest when none of them declares the wanted
    // name. That fallback is fine for browsing, but generating a spec from it
    // silently ships the wrong project (e.g. building a compiled-artifact-only
    // wheel's monorepo checkout, which builds its unrelated top-level
    // package). Refuse instead of guessing, so a mismatch surfaces here
    // rather than several minutes later in review.
    let normalize = |n: &str| n.to_lowercase().replace(['-', '.'], "_");
    let wanted = normalize(&config.pkg_name);
    let declared_names: Vec<&str> = info
        .project_name
        .iter()
        .map(String::as_str)
        .chain(info.subprojects.iter().filter_map(|s| s.name.as_deref()))
        .collect();
    // Skip the mismatch guard when the archive is a Python-hijacked
    // C/C++ project: the "declared name doesn't match wanted" condition
    // is true BY CONSTRUCTION in that case (it's the hijack's own
    // trigger), and letting it fall through sends a C/C++ project to
    // `try_wheel_fallback` which cannot possibly help.
    if !is_python_hijack
        && (info.python_packaging || info.cargo_manifest)
        && !declared_names.iter().any(|n| normalize(n) == wanted)
    {
        let found = if declared_names.is_empty() {
            "no named projects at all".to_string()
        } else {
            declared_names.join(", ")
        };
        let mismatch_reason = format!(
            "{url} does not declare a project named {wanted:?} (found: {found}). \
             This usually means {} is binary-only upstream and this repository \
             is not its real source.",
            config.pkg_name
        );
        // Don't just die here: a repo-guessing heuristic that "verified" (the
        // repo exists) but picked the wrong artifact for this specific PyPI
        // release is exactly what convention 28 Exception 4's second
        // paragraph covers when the Review agent finds it — the fix is the
        // same either way (repackage the upstream wheel), so do it now
        // instead of hard-failing and waiting for a human to notice the
        // error message's own advice ("check whether ... a wheel"). The
        // wrongly-fetched archive is no longer needed either way.
        let _ = std::fs::remove_file(&tarball);
        // Discard the picked wheel info here: the provenance file only
        // makes sense when the switch is the result of a deliberate agent
        // `UseUpstreamWheel`, where the audit trail has a specific `why`
        // to record. This call site is the generation path's automatic
        // retry on a wrongly-guessed repo (no agent decision was made),
        // so there is nothing to attest beyond "guess was wrong, we fell
        // back" and the warning above the caller already covers it.
        return match try_wheel_fallback(config, checkout_dir, spec_path, step) {
            Ok(_wheel) => Ok(GenSpecResult {
                sdist_top_level: String::new(),
                is_maturin: false,
                is_rust: false,
                from_repo_archive: false,
            }),
            Err(e) => Err(GenSpecError::Other(format!(
                "{mismatch_reason} Tried repackaging the upstream wheel instead, but {e} \
                 (retry with a different --source-url if one exists)."
            ))),
        };
    }

    // Treat a Python-hijacked archive as C/C++ before any Python/Rust
    // branch has a chance to claim it — the pyproject.toml here is a
    // scripts sidecar, not the primary build system.
    if info.python_packaging && !is_python_hijack {
        // py2pack needs PyPI metadata for the summary, license and deps. When
        // the project is not on PyPI at all there is nothing to fetch, and the
        // agent fills those in from the source during the review turn.
        let pypi_json = match fetch_pypi_json(&config.pkg_name, &config.version) {
            Ok(json) => json.unwrap_or_default(),
            Err(e) => {
                // Not "not on PyPI": the spec would silently lack its license,
                // summary and dependencies. Say so; the review turn still fills
                // them in from the source.
                ap_core::diag!("  warning: {e}; generating the spec without PyPI metadata");
                String::new()
            }
        };
        let result =
            generate_repo_python_spec(config, &tarball, &info, &pypi_json, spec_path, step)?;
        record_archive_digest(spec_path, &url, &digest);
        return Ok(result);
    }

    if info.cargo_manifest {
        // The Rust path re-fetches through `osc service ra`, which is what
        // produces the vendored sources OBS builds from.
        let _ = std::fs::remove_file(&tarball);
        return generate_rust_spec(config, repo_url, spec_path, step);
    }

    if info.cmake_at_root || info.autotools_at_root {
        // Unlike Rust, a plain C/C++ project vendors nothing through OBS
        // services — the archive already downloaded is the real Source0.
        let build = if info.cmake_at_root {
            crate::spec::CBuildSystem::Cmake
        } else {
            crate::spec::CBuildSystem::Autotools
        };
        let top_level = info
            .top_level_dir
            .clone()
            .unwrap_or_else(|| format!("{name}-{}", config.version));
        let spec_text = crate::spec::c_project_skeleton(
            &config.pkg_name,
            &config.version,
            repo_url,
            &top_level,
            build,
        );
        let spec_text = crate::spec::strip_ap_fix_comments(&spec_text);
        ap_core::fsx::write_atomic(spec_path, &spec_text)
            .map_err(|e| GenSpecError::Other(format!("writing spec failed: {e}")))?;
        record_archive_digest(spec_path, &url, &digest);
        write_or_warn(step.file("spec.generated"), &spec_text);
        return Ok(GenSpecResult {
            sdist_top_level: top_level,
            is_maturin: false,
            is_rust: false,
            // Not the PyPI-specific repo-archive case (convention 23) — a
            // plain C/C++ project has no PyPI angle at all. Its own guidance
            // is baked into the skeleton's convention 29 comment instead.
            from_repo_archive: false,
        });
    }

    Err(GenSpecError::Other(format!(
        "{url} contains no pyproject.toml, setup.py, Cargo.toml, CMakeLists.txt \
         or configure.ac, so auto-package cannot tell how to build it"
    )))
}
