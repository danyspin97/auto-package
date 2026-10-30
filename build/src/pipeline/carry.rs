//! Carrying a spec forward from the previous version: the repo and service forms.

use super::*;
use crate::pipeline::prompt_notes::*;

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
) -> Result<GenSpecResult, String> {
    use ap_ecosystem::{pypi, sdist};

    if config.source_repo.is_some() {
        return carry_forward_repo_spec(config, checkout_dir, spec_path, step);
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
        &crate::spec::expanded_sources(&bumped),
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
) -> Result<GenSpecResult, String> {
    let old = std::fs::read_to_string(spec_path).map_err(|e| format!("reading the spec: {e}"))?;
    let mut bumped = crate::spec::set_version(&old, &config.version)
        .ok_or("its Version: is not a plain value")?;
    if crate::spec::expanded_sources(&bumped).is_empty() {
        // The source has no web address: a `_service` fetches it, or the
        // tarball was committed by hand.
        let repo = config
            .source_repo
            .as_deref()
            .ok_or("the spec's sources have no web address to download from")?;
        if checkout_dir.join("_service").is_file() {
            return carry_forward_service_spec(config, repo, checkout_dir, spec_path, step, bumped);
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
    let sources = crate::spec::expanded_sources(&bumped);
    if sources.is_empty() {
        return Err("the spec's sources have no web address to download from".to_string());
    }
    // Only a source whose address changes with the version is fetched again;
    // the others (a keyring, a rpmlintrc) stay as they are.
    let old_sources = crate::spec::expanded_sources(&old);
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
            && is_superseded_source_file(old_file, &sources)
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

/// [`carry_forward_repo_spec`] for a package whose source a `tar_scm` service
/// (or a `cargo_vendor` one given a `url`) fetches: the service's `revision` moves to the new tag, `osc service ra`
/// makes the tarball, and the old one goes. The spec keeps everything else.
fn carry_forward_service_spec(
    config: &PipelineConfig,
    repo: &str,
    checkout_dir: &Path,
    spec_path: &Path,
    step: &StepDir,
    bumped: String,
) -> Result<GenSpecResult, String> {
    let service_path = checkout_dir.join("_service");
    let service =
        std::fs::read_to_string(&service_path).map_err(|e| format!("reading _service: {e}"))?;
    // `obs_scm` makes an `.obscpio`, which nothing below handles: only `tar_scm`
    // and `cargo_vendor` with a `url`, which fetches the source itself.
    if service_revision_block(&service).is_none() {
        return Err(
            "the _service is not a tar_scm or url cargo_vendor one, so the source cannot be moved"
                .into(),
        );
    }
    let tag = crate::sources::tag_for_version(repo, &config.version)
        .ok_or_else(|| format!("{repo} lists no tag for {}", config.version))?;
    let moved = with_service_revision(&service, &tag)
        .ok_or("the _service has no revision to move to the new tag")?;
    let old_spec =
        std::fs::read_to_string(spec_path).map_err(|e| format!("reading the spec: {e}"))?;
    let old_version = crate::spec::parse_header(&old_spec).map(|h| h.version);
    let before: std::collections::HashSet<String> = tarballs_in(checkout_dir);
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
        run_service_with_retries(checkout_dir, Emit::quiet())
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

/// The tarballs (by file name) in `dir`.
fn tarballs_in(dir: &Path) -> std::collections::HashSet<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            [".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar"]
                .iter()
                .any(|s| n.ends_with(s))
        })
        .collect()
}

/// Where the service that fetches the source starts in `service`: the `tar_scm`
/// service, else a `cargo_vendor` one that has a `url` parameter (it fetches the
/// source and vendors it in one go; without a `url` it only vendors).
fn service_revision_block(service: &str) -> Option<usize> {
    if let Some(at) = service.find("name=\"tar_scm\"") {
        return Some(at);
    }
    let at = service.find("name=\"cargo_vendor\"")?;
    let block_end = service[at..]
        .find("</service>")
        .map_or(service.len(), |i| at + i);
    service[at..block_end]
        .contains("<param name=\"url\">")
        .then_some(at)
}

/// `service` with the `revision` parameter of the service that fetches the
/// source set to `tag`; `None` when it has none.
pub(super) fn with_service_revision(service: &str, tag: &str) -> Option<String> {
    let open = "<param name=\"revision\">";
    let scm = service_revision_block(service)?;
    let start = scm + service[scm..].find(open)? + open.len();
    let end = start + service[start..].find("</param>")?;
    Some(format!(
        "{}{}{}",
        &service[..start],
        crate::obs_xml::escape(tag),
        &service[end..]
    ))
}

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
    for entry in entries.flatten() {
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
            // Factory's `<pkg>-rpmlintrc`; the wheel's own is `<pkg>.rpmlintrc`.
            || name.ends_with("-rpmlintrc");
        if recipe_only {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
