//! Carrying a spec forward from the previous version: the repo and service forms.

use super::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::service_source::*;
use crate::pipeline::sibling_specs::*;

/// Bump the spec already in the checkout to `config.version` without
/// regenerating it: set its `Version:`, fetch the new PyPI sdist in place of
/// the old tarball, and fix the `%autosetup -n` directory. `Err` says why the
/// recipe cannot be carried (a repository source, a wheel-only release, a
/// monorepo, a computed version) and the caller generates a spec instead.
pub(super) fn carry_forward_spec(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    reporter: &dyn Reporter,
) -> Result<GenSpecResult, String> {
    use ap_ecosystem::{pypi, sdist};

    if config.source_repo.is_some() {
        return carry_forward_repo_spec(config, checkout_dir, spec_path, step, reporter);
    }
    let old = std::fs::read_to_string(spec_path).map_err(|e| format!("reading the spec: {e}"))?;
    let old_version = crate::spec::parse_header(&old)
        .map(|h| h.version)
        .ok_or("the spec has no Name/Version")?;
    let bumped = crate::spec::set_version(&old, &config.version)
        .ok_or("its Version: is not a plain value")?;

    let json = fetch_pypi_json(&config.pkg_name, &config.version)?
        .ok_or_else(|| format!("PyPI has no {} {}", config.pkg_name, config.version))?;
    let release = pypi::parse_release(&json).map_err(|e| format!("no usable sdist: {e}"))?;
    let tarball = release
        .sdist
        .fetch(checkout_dir)
        .map_err(|e| format!("downloading {}: {e}", release.sdist.url))?;
    let info = sdist::inspect(&tarball).map_err(|e| format!("inspecting the sdist: {e}"))?;
    if !info.subprojects.is_empty() {
        return Err("the sdist holds several projects".to_string());
    }

    // The old source goes once the new spec is written: the spec names the new
    // one. Until then the old file is kept, so a failed write leaves the
    // checkout as it was.
    let stale = superseded_tarball(
        checkout_dir,
        &old_version,
        &tarball,
        &[
            crate::spec::expanded_sources(&bumped),
            sibling_spec_sources(checkout_dir, spec_path),
        ]
        .concat(),
    );

    let top = info
        .top_level_dir
        .clone()
        .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));
    let bumped = crate::spec::fix_autosetup_dir(&bumped, &top).unwrap_or(bumped);
    ap_core::fsx::write_atomic(spec_path, &bumped).map_err(|e| format!("writing the spec: {e}"))?;
    if let Some(stale) = stale
        && let Err(e) = std::fs::remove_file(&stale)
    {
        ap_core::diag!("  could not remove the old source {}: {e}", stale.display());
    }
    write_or_warn(step.file("spec.generated"), &bumped);
    Ok(GenSpecResult {
        sdist_top_level: top,
        is_maturin: info.build_system.is_maturin(),
        is_rust: false,
        from_repo_archive: false,
    })
}

/// Moves the sources whose address names the old version's tag literally (no
/// macro for the version) to the archive of the new version's tag, given by
/// `archive_of`. Such an address is the same after the version change, so
/// nothing would be fetched for it. Only the `SourceN:` line that declares such
/// a source is rewritten, and `archive_of` is asked once for each version.
/// `Err` when the new tag has no archive or the address is written with a
/// macro, which the line does not hold.
pub(super) fn move_literal_tag_archives(
    old_version: &str,
    spec: &str,
    new_version: &str,
    archive_of: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if old_version.is_empty() {
        return Ok(spec.to_string());
    }
    let named_old: Vec<(usize, String)> = crate::spec::expanded_source_lines(spec)
        .into_iter()
        .filter(|(_, source)| source.url.contains(old_version))
        .map(|(at, source)| (at, source.url))
        .collect();
    if named_old.is_empty() {
        return Ok(spec.to_string());
    }
    let old_archive = archive_of(old_version);
    let moving: Vec<(usize, String)> = named_old
        .into_iter()
        .filter(|(_, url)| old_archive.as_deref() == Some(url.as_str()))
        .collect();
    if moving.is_empty() {
        return Ok(spec.to_string());
    }
    let new_url = archive_of(new_version).ok_or_else(|| {
        format!("the repository lists no tag for {new_version}, so there is no archive to fetch")
    })?;
    let mut lines: Vec<String> = spec.split_inclusive('\n').map(str::to_string).collect();
    for (at, url) in moving {
        lines[at] = replace_source_address(&lines[at], &url, &new_url)?;
    }
    Ok(lines.concat())
}

/// `line`, a `SourceN:` line that declares the literal address `old`, with that
/// address replaced by `new`. Only the value of the line is changed, so a
/// comment or another address that contains `old` is left alone. `Err` when the
/// line writes the address with a macro, so the text does not hold `old`.
fn replace_source_address(line: &str, old: &str, new: &str) -> Result<String, String> {
    let literal = crate::spec::parse_source_line(line, &["source"], str::to_string)
        .is_some_and(|source| source.url == old);
    let Some((head, value)) = line.split_once(':').filter(|_| literal) else {
        return Err(format!(
            "the address {old} cannot be rewritten in `{}`, which writes it with a macro",
            line.trim()
        ));
    };
    // The address is the first thing in the value, so the first match is it.
    Ok(format!("{head}:{}", value.replacen(old, new, 1)))
}

/// Whether the old source file `old_file` may be removed after a new spec
/// replaces it: only when no source of `new` is named that file.
pub(super) fn is_superseded_source_file(old_file: &str, new: &[crate::spec::SourceEntry]) -> bool {
    !new.iter().any(|s| s.file == old_file)
}

/// [`carry_forward_spec`] for a package built from a source repository's
/// archive (a git forge release, a tarball in a download directory): set the
/// new `Version:`, download what the spec's web sources are at that version in
/// place of the old files, and fix the `%autosetup -n` directory. Nothing is
/// regenerated, so the recipe of a package that is not Python is never mistaken
/// for one: a C project that ships `setup.py` for its Python bindings (dtc's
/// `pylibfdt`) keeps the build it has. `Err` says why the recipe cannot be
/// carried: a source with no address, which a `_service` fetches.
fn carry_forward_repo_spec(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    reporter: &dyn Reporter,
) -> Result<GenSpecResult, String> {
    let old = std::fs::read_to_string(spec_path).map_err(|e| format!("reading the spec: {e}"))?;
    let mut bumped =
        crate::spec::set_version_and_tag(&old, &config.version, config.upstream_tag.as_deref())?;
    // The source now names the tag by a define the version does not spell:
    // say so in the changes, beside the version bump's own entry.
    if let Some(tag) = config.upstream_tag.as_deref()
        && !old.contains("upstream_version")
        && bumped.contains("%define upstream_version")
    {
        warn_if_err(
            reporter,
            "adding the .changes entry for the upstream tag",
            osc::add_changes_entry(
                checkout_dir,
                &format!(
                    "Build the source from the upstream tag {tag}: {} is its version \
                     with the revision after a dot",
                    config.version
                ),
                Emit::new(reporter, step.id),
            ),
        );
    }
    if crate::spec::expanded_sources(&bumped).is_empty() {
        // The source has no web address: a `_service` fetches it, or the
        // tarball was committed by hand.
        let repo = config
            .source_repo
            .as_deref()
            .ok_or("the spec's sources have no web address to download from")?;
        if checkout_dir.join("_service").is_file() {
            return carry_forward_service_spec(
                config,
                repo,
                checkout_dir,
                spec_path,
                step,
                bumped,
                reporter,
            );
        }
        if bumped
            .lines()
            .find(|l| {
                l.strip_prefix("Source").is_some_and(|r| {
                    r.trim_start_matches(|c: char| c.is_ascii_digit())
                        .starts_with(':')
                })
            })
            .is_some_and(|l| l.contains("://"))
        {
            return Err(
                "the first Source: has an address with a macro that cannot be expanded".into(),
            );
        }
        let url = crate::sources::tag_archive_url(repo, &config.version).ok_or_else(|| {
            format!(
                "{repo} lists no tag for {}, so there is no archive to fetch",
                config.version
            )
        })?;
        // The address becomes part of the recipe, under the file name the spec
        // already unpacks, so every later update has one to follow.
        bumped = crate::spec::set_first_source(
            &bumped,
            &format!("{url}#/%{{name}}-%{{version}}.tar.gz"),
        )
        .ok_or("the spec has no Source: line")?;
    }
    if let (Some(repo), Some(old_version)) = (
        config.source_repo.as_deref(),
        crate::spec::parse_header(&old).map(|h| h.version),
    ) {
        bumped = move_literal_tag_archives(&old_version, &bumped, &config.version, |v| {
            crate::sources::tag_archive_url(repo, v)
        })?;
    }
    let sources = crate::spec::expanded_sources(&bumped);
    if sources.is_empty() {
        return Err("the spec's sources have no web address to download from".to_string());
    }
    // Only a source whose address changes with the version is fetched again;
    // the others (a keyring, a rpmlintrc) stay as they are.
    let old_sources = crate::spec::expanded_sources(&old);
    // An old file a sibling spec of the package still names stays.
    let kept_by_all = [
        sources.clone(),
        sibling_spec_sources(checkout_dir, spec_path),
    ]
    .concat();
    // Every source fetched again, in spec order: the first one that is an
    // archive gives the top directory (a changed `.asc` signature does not).
    let mut fetched: Vec<PathBuf> = Vec::new();
    // Old files replaced by this run, removed after the spec is written.
    let mut superseded: Vec<String> = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        if old_sources.get(i).is_some_and(|o| o.url == source.url) {
            continue;
        }
        let dest = checkout_dir.join(&source.file);
        crate::sources::fetch_archive_to(&source.url, &dest)
            .map_err(|e| format!("downloading {}: {e}", source.url))?;
        fetched.push(dest);
        // The old file this slot had is removed only when no source of the new
        // spec is named that: sources can be reordered or shifted, and the
        // file at this index may be one a later new source just needs.
        if let Some(old_file) = old_sources.get(i).map(|o| &o.file)
            && is_superseded_source_file(old_file, &kept_by_all)
        {
            superseded.push(old_file.clone());
        }
    }
    if fetched.is_empty() {
        return Err("no source of the spec changes with the version".to_string());
    }
    let top = fetched.iter().find_map(|path| {
        ap_ecosystem::sdist::inspect(path)
            .ok()
            .and_then(|info| info.top_level_dir)
    });
    let bumped = match &top {
        Some(top) => crate::spec::fix_autosetup_dir(&bumped, top).unwrap_or(bumped),
        None => bumped,
    };
    ap_core::fsx::write_atomic(spec_path, &bumped).map_err(|e| format!("writing the spec: {e}"))?;
    // The superseded files go only now that every fetch and the spec write
    // have succeeded: a failure above must leave the old sources in place.
    for old_file in &superseded {
        let _ = std::fs::remove_file(checkout_dir.join(old_file));
    }
    write_or_warn(step.file("spec.generated"), &bumped);
    Ok(GenSpecResult {
        sdist_top_level: top
            .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version)),
        is_maturin: false,
        is_rust: false,
        from_repo_archive: true,
    })
}

/// [`carry_forward_repo_spec`] for a package whose source a service supplies: a
/// `tar_scm` service (or a `cargo_vendor` one given a `url`) fetches it, so the
/// service's `revision` moves to the new tag, `osc service ra` makes the
/// tarball, and the old one goes. An `obs_scm` service is made by OBS at build
/// time: only its `revision` and the spec's `Version:` move, see
/// [`carry_forward_obs_scm_spec`]. The spec keeps everything else.
fn carry_forward_service_spec(
    config: &PipelineConfig,
    repo: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    bumped: String,
    reporter: &dyn Reporter,
) -> Result<GenSpecResult, String> {
    let service_path = checkout_dir.join("_service");
    let service =
        std::fs::read_to_string(&service_path).map_err(|e| format!("reading _service: {e}"))?;
    let old_spec =
        std::fs::read_to_string(spec_path).map_err(|e| format!("reading the spec: {e}"))?;
    let old_version = crate::spec::parse_header(&old_spec).map(|h| h.version);
    let plan = plan_service_carry(&service, &config.version, old_version.as_deref(), || {
        crate::sources::tag_for_version(repo, &config.version)
            .ok_or_else(|| format!("{repo} lists no tag for {}", config.version))
    })?;
    if let Some(why) = &plan.unchecked {
        report_version_unchecked(reporter, step.id, &config.rpm_name, why);
    }
    if plan.kind == SourceService::BuildTime {
        carry_forward_obs_scm_spec(&service_path, &service, &plan.moved, spec_path, &bumped)?;
        write_or_warn(step.file("spec.generated"), &bumped);
        return Ok(GenSpecResult {
            // Nothing is extracted, so the top directory is a guess; nothing
            // trusts it while `source_made_at_build_time` holds.
            sdist_top_level: format!("{}-{}", config.pkg_name.to_lowercase(), config.version),
            is_maturin: false,
            is_rust: false,
            from_repo_archive: false,
        });
    }
    let moved = plan.moved;
    let before: std::collections::HashSet<String> = tarballs_in(checkout_dir);
    let sibling_files: Vec<String> = sibling_spec_sources(checkout_dir, spec_path)
        .into_iter()
        .map(|s| s.file)
        .collect();
    // Both files go back as they were when anything below fails: a half-moved
    // checkout would be taken for a finished one by the next run.
    let restore = || {
        let _ = std::fs::write(&service_path, &service);
        let _ = std::fs::write(spec_path, &old_spec);
    };
    let result = (|| -> Result<(String, PathBuf), String> {
        ap_core::fsx::write_atomic(&service_path, &moved)
            .map_err(|e| format!("writing _service: {e}"))?;
        ap_core::fsx::write_atomic(spec_path, &bumped)
            .map_err(|e| format!("writing the spec: {e}"))?;
        run_service_with_retries(
            checkout_dir,
            &config.rpm_name,
            obs_baseline_dir(config).ok().as_deref(),
            Emit::quiet(),
        )
        .map_err(|e| format!("running the _service: {e}"))?;
        // The service writes `Version:` from the tarball (`set_version`) when it
        // is enabled; the version asked for is what the spec must say.
        let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| bumped.clone());
        let keep = crate::spec::set_version(&after, &config.version).unwrap_or(after);
        ap_core::fsx::write_atomic(spec_path, &keep)
            .map_err(|e| format!("writing the spec: {e}"))?;
        let made = find_tarball(checkout_dir, &config.version)
            .ok_or("the _service made no tarball for the new version")?;
        Ok((keep, made))
    })();
    let (keep, made) = match result {
        Ok(done) => done,
        Err(e) => {
            restore();
            return Err(e);
        }
    };
    // The tarball of the release being replaced goes; an archive that does not
    // carry the old version in its name (a vendor bundle) is left alone.
    if let Some(old_version) = old_version.filter(|v| *v != config.version) {
        for old in before {
            if old.contains(&old_version)
                && !old.contains(&config.version)
                && made.file_name().is_none_or(|n| n.to_string_lossy() != old)
                && !sibling_files.contains(&old)
            {
                let _ = std::fs::remove_file(checkout_dir.join(&old));
            }
        }
    }
    let top = ap_ecosystem::sdist::inspect(&made)
        .ok()
        .and_then(|info| info.top_level_dir)
        .unwrap_or_else(|| format!("{}-{}", config.pkg_name.to_lowercase(), config.version));
    write_or_warn(step.file("spec.generated"), &keep);
    Ok(GenSpecResult {
        sdist_top_level: top,
        is_maturin: false,
        is_rust: false,
        from_repo_archive: true,
    })
}

/// Says that the `Version:` of `pkg` could not be checked against its tag: a
/// warning where the run reports it, and a record the run summary lists.
pub(super) fn report_version_unchecked(
    reporter: &dyn Reporter,
    step: ap_core::event::StepId,
    pkg: &str,
    why: &str,
) {
    reporter.report(&Event::Warning {
        step: Some(step),
        message: format!("{pkg}: {why}"),
    });
    reporter.report(&Event::VersionUnchecked {
        pkg: pkg.to_string(),
        reason: why.to_string(),
    });
}

/// What the `_service` of a package whose source a service supplies becomes.
pub(super) struct ServicePlan {
    /// The `_service` with its source service's `revision` moved to the tag.
    pub(super) moved: String,
    pub(super) kind: SourceService,
    /// Why the `Version` the tag gives could not be checked, when it could not.
    pub(super) unchecked: Option<String>,
}

/// Plans the move of `service` to `version`: which service supplies the source,
/// and the `_service` with its `revision` set to the tag `tag_of` finds. Every
/// refusal that needs no network comes before `tag_of` runs.
pub(super) fn plan_service_carry(
    service: &str,
    version: &str,
    old_version: Option<&str>,
    tag_of: impl FnOnce() -> Result<String, String>,
) -> Result<ServicePlan, String> {
    let masked = mask_xml_comments(service);
    let (scm, kind) = source_service(&masked).ok_or(
        "the _service is not a tar_scm, obs_scm or url cargo_vendor one, so the source cannot be moved",
    )?;
    revision_span(&masked, scm).ok_or("the _service has no revision to move to the new tag")?;
    let mut unchecked = None;
    let mut base = service.to_string();
    if kind == SourceService::BuildTime {
        refuse_manual_vendoring(&masked)?;
        // A literal versionformat gives every tag the same version; OBS takes it
        // from the tag once it is @PARENT_TAG@. That is refused, before any
        // lookup, where the tag could not be checked afterwards.
        let why = build_time_unverifiable(&source_params(service, &masked, scm));
        base = with_parent_tag_version_format(service, &masked, scm, old_version, why.as_deref())?;
        unchecked = why;
    }
    let tag = tag_of()?;
    let moved = with_service_revision(&base, &tag)
        .ok_or("the _service has no revision to move to the new tag")?;
    if kind == SourceService::BuildTime && unchecked.is_none() {
        // The check runs on the moved service, as OBS will read it.
        let moved_masked = mask_xml_comments(&moved);
        let (moved_scm, _) = source_service(&moved_masked)
            .ok_or("the _service is not a tar_scm, obs_scm or url cargo_vendor one")?;
        unchecked = check_build_time_version(
            &source_params(&moved, &moved_masked, moved_scm),
            &tag,
            version,
        )?;
    }
    Ok(ServicePlan {
        moved,
        kind,
        unchecked,
    })
}

/// Writes the moved `_service` and the spec with its `Version:` bumped, both or
/// neither: a failed spec write puts `_service` back. Nothing is run or
/// downloaded, since OBS makes the source when the package is built.
pub(super) fn carry_forward_obs_scm_spec(
    service_path: &Path,
    service: &str,
    moved: &str,
    spec_path: &Path,
    bumped: &str,
) -> Result<(), String> {
    ap_core::fsx::write_atomic(service_path, moved)
        .map_err(|e| format!("writing _service: {e}"))?;
    if let Err(e) = ap_core::fsx::write_atomic(spec_path, bumped) {
        let _ = std::fs::write(service_path, service);
        return Err(format!("writing the spec: {e}"));
    }
    Ok(())
}

/// Refuses an `obs_scm` source next to a vendoring service that OBS does not
/// make at build time: its archive would stay the old one in the checkout and
/// the package would build the new source with the old dependencies.
fn tarballs_in(dir: &Path) -> std::collections::HashSet<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_archive_name(n))
        .collect()
}

/// How the service that supplies the source gets it.

/// The PyPI JSON of this package's version.
pub(super) fn release_json_for(config: &PipelineConfig) -> Result<String, GenSpecError> {
    let pypi_url = pypi_json_url(&config.pkg_name, &config.version);
    crate::http::read_text(crate::http::get_with_retries(&pypi_url).map_err(|e| {
        GenSpecError::Other(format!(
            "PyPI fetch failed for {} {}: {e}",
            config.pkg_name, config.version
        ))
    })?)
    .map_err(|e| {
        GenSpecError::Other(format!(
            "PyPI read failed for {} {}: {e}",
            config.pkg_name, config.version
        ))
    })
}

/// What a source recipe leaves in a checkout that a wheel repackage must not
/// keep: the service that fetched the sources, the flavors and build
/// constraints of a compile, the patches and archives of the old source, and
/// the Factory recipe's own rpmlint file. They are deleted here and go from OBS
/// with the commit. `_link` stays (it only ties the package to its origin).
pub(super) fn purge_source_recipe(checkout_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(checkout_dir) else {
        return;
    };
    let entries: Vec<_> = entries.flatten().collect();
    // The wheel's own rpmlintrc is named like Factory's (`<pkg>-rpmlintrc`):
    // the one a spec names as a source stays. An older `<pkg>.rpmlintrc` that no
    // spec names goes too.
    let specs: Vec<String> = entries
        .iter()
        .filter(|e| e.path().extension().is_some_and(|x| x == "spec"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    let named = |name: &str| specs.iter().any(|s| osc::spec_names_source(s, name));
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_file() {
            continue;
        }
        let recipe_only = matches!(
            name.as_str(),
            "_service" | "_servicedata" | "_multibuild" | "_constraints" | "series"
        ) || name.ends_with(".patch")
            || name.ends_with(".diff")
            || name.ends_with(".obscpio")
            || name.ends_with(".orig")
            || [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".zip", ".crate"]
                .iter()
                .any(|s| name.ends_with(s))
            // Factory's `<pkg>-rpmlintrc`, unless the wheel's spec names it.
            || ((name.ends_with("-rpmlintrc") || name.ends_with(".rpmlintrc")) && !named(&name));
        if recipe_only {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
