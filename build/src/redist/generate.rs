use super::*;

/// The spec for a component, the archive it was made from and the rpmlintrc.
#[derive(Debug)]
pub struct Generated {
    pub spec: String,
    pub rpmlintrc_file: String,
    pub rpmlintrc: String,
    /// The archive in the checkout directory.
    pub archive: PathBuf,
    pub url: String,
    pub sha256: String,
}

/// Everything for the package `rpm_name`: the manifest's component and the
/// archive for `arch`, downloaded into `checkout_dir` and verified, its member
/// list, and the spec and rpmlintrc made from them. `version` is what the
/// caller believes the package version is; it must be the manifest's.
pub fn generate(
    source: &dyn Source,
    rpm_name: &str,
    version: &str,
    arch: &str,
    checkout_dir: &Path,
) -> Result<Generated, String> {
    let key = component(rpm_name).ok_or_else(|| format!("{rpm_name} is not a CUDA component"))?;
    if key == FILESYSTEM_KEY {
        return generate_filesystem(source, rpm_name, version, checkout_dir);
    }
    let product = product_for(key);
    let platform = platform_for(arch)?;
    let manifest = source.manifest(product)?;
    let comp = find_component(&manifest, key)?;
    if !version.is_empty() && comp.version != version {
        return Err(format!(
            "{rpm_name} was to be built at {version}, but NVIDIA's current {key} is {}: the \
             version of a CUDA component is the manifest's, not a wheel's",
            comp.version
        ));
    }
    let cuda_major = product.per_cuda_major().then_some(CUDNN_CUDA_MAJOR);
    let archive = comp.archive_for(platform, cuda_major).ok_or_else(|| {
        format!(
            "{key} {} has no {platform} archive{}",
            comp.version,
            cuda_major
                .map(|m| format!(" for CUDA {m}"))
                .unwrap_or_default()
        )
    })?;
    let url = archive.url(&manifest.base_url);
    let filename = archive
        .relative_path
        .rsplit('/')
        .next()
        .filter(|f| {
            f.ends_with(".tar.xz")
                && f.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
        })
        .ok_or_else(|| format!("unusable archive path {:?}", archive.relative_path))?
        .to_string();
    let dest = checkout_dir.join(&filename);
    fetch_verified(source, &url, &dest, &archive.sha256)?;
    let files = ap_ecosystem::sdist::list_archive_members(&dest)
        .map_err(|e| format!("listing {filename}: {e}"))?;
    // An empty package must not be reported as built.
    crate::nvidia_spec::check_layout(&files)
        .map_err(|why| format!("{filename} cannot be packaged: {why}"))?;

    let input = RedistSpecInput {
        component_key: key.to_string(),
        name: comp.name.clone(),
        version: comp.version.clone(),
        cuda_release: cuda_release(source, product)?,
        license: comp.license.clone(),
        archive_filename: filename,
        archive_url: url.clone(),
        arch: arch.to_string(),
        files,
    };
    let rpmlintrc = crate::nvidia_spec::redist_rpmlintrc(&input);
    let spec = crate::nvidia_spec::redist_component_spec(&input);
    // The same note a repository archive gets, so the commit keeps a digest of
    // what the spec was made from.
    let spec =
        crate::spec_tidy::with_archive_note(&spec, &url, &archive.sha256.to_ascii_lowercase());
    Ok(Generated {
        spec,
        rpmlintrc_file: format!("{rpm_name}-rpmlintrc"),
        rpmlintrc,
        archive: dest,
        url,
        sha256: archive.sha256.to_ascii_lowercase(),
    })
}

/// The spec of a `cuda-<major>-<minor>-filesystem` package: no archive, so
/// nothing is downloaded and `archive` names a file that is not there (no stale
/// archive matches it).
pub(super) fn generate_filesystem(
    source: &dyn Source,
    rpm_name: &str,
    version: &str,
    checkout_dir: &Path,
) -> Result<Generated, String> {
    let mm =
        filesystem_mm(rpm_name).ok_or_else(|| format!("{rpm_name} is not a CUDA component"))?;
    let have = filesystem_version(source, &mm)?;
    if !version.is_empty() && have != version {
        return Err(format!(
            "{rpm_name} was to be built at {version}, but NVIDIA's current CUDA release is \
             {have}: the version of a CUDA component is the manifest's"
        ));
    }
    Ok(Generated {
        spec: crate::nvidia_spec::redist_filesystem_spec(&mm, &have),
        rpmlintrc_file: format!("{rpm_name}-rpmlintrc"),
        rpmlintrc: crate::nvidia_spec::redist_filesystem_rpmlintrc(),
        archive: checkout_dir.join("no-archive"),
        url: String::new(),
        sha256: String::new(),
    })
}

/// Remove the archives of other releases of this component from `checkout_dir`
/// (`cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz` once the spec names
/// 13.5.0): the commit to OBS drops them with the old source.
pub fn remove_stale_archives(checkout_dir: &Path, keep: &Path) {
    let Some(keep_name) = keep.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    // `<key>-linux-<arch>-<version>-archive.tar.xz`: the part up to and
    // including the platform identifies the component and the platform, so a
    // release for another platform (`-linux-sbsa-`) is left alone.
    let Some((stem, rest)) = keep_name.split_once("-linux-") else {
        return;
    };
    let Some((platform, _)) = rest.split_once('-') else {
        return;
    };
    let prefix = format!("{stem}-linux-{platform}-");
    let Ok(entries) = std::fs::read_dir(checkout_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != keep_name
            && name.starts_with(&prefix)
            && name.ends_with("-archive.tar.xz")
            && entry.path().is_file()
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
