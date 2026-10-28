use super::*;

/// What the mode takes from the network.
pub trait Source {
    /// The latest manifest of `product`.
    fn manifest(&self, product: Product) -> Result<Manifest, String>;
    /// Stream `url` to `dest`, returning the SHA-256 of what was written.
    fn download(&self, url: &str, dest: &Path) -> Result<String, String>;
}

/// NVIDIA's servers. Each product's manifest is fetched once per process.
pub struct Network;

pub(super) static MANIFESTS: OnceLock<Mutex<Vec<(Product, Manifest)>>> = OnceLock::new();

impl Source for Network {
    fn manifest(&self, product: Product) -> Result<Manifest, String> {
        let cache = MANIFESTS.get_or_init(Default::default);
        // Held across the fetch: threads asking for the same manifest wait for
        // the first instead of each downloading it.
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, m)) = cache.iter().find(|(p, _)| *p == product) {
            return Ok(m.clone());
        }
        let m = nvidia_redist::fetch_manifest(product, None)
            .map_err(|e| format!("reading NVIDIA's {product:?} manifest: {e}"))?;
        cache.push((product, m.clone()));
        Ok(m)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<String, String> {
        crate::http::download_file(url, dest)
            .map(|d| d.sha256)
            .map_err(|e| format!("downloading {url}: {e}"))
    }
}

pub(super) fn find_component<'a>(
    manifest: &'a Manifest,
    key: &str,
) -> Result<&'a nvidia_redist::Component, String> {
    manifest
        .components
        .iter()
        .find(|c| c.key == key)
        .ok_or_else(|| {
            format!(
                "NVIDIA's {} manifest ({}) has no component {key}",
                manifest.product, manifest.release_label
            )
        })
}

/// The manifest version of the component `name` stands for. `None` when the
/// mode is off or `name` is not a component.
pub fn latest_version(source: &dyn Source, name: &str) -> Option<Result<String, String>> {
    let key = component(name)?;
    if key == FILESYSTEM_KEY {
        return Some(filesystem_version(source, &filesystem_mm(name)?));
    }
    Some(
        source
            .manifest(product_for(key))
            .and_then(|m| find_component(&m, key).map(|c| c.version.clone())),
    )
}

/// The version of the filesystem package of CUDA `mm` (`13.4`): the CUDA
/// release the manifest names when it is that one, `<mm>.0` otherwise (the
/// cuDNN prefix of a CUDA release whose manifest is another).
pub(super) fn filesystem_version(source: &dyn Source, mm: &str) -> Result<String, String> {
    let label = source.manifest(Product::Cuda)?.release_label;
    let mut parts = label.split('.');
    let same = format!(
        "{}.{}",
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("")
    );
    Ok(if same == mm { label } else { format!("{mm}.0") })
}

/// Replace the version of every node that is a component with the manifest's:
/// the wheel's `13.1.1.3` is not the package's `13.8.0.4`, and everything that
/// asks "is this version built" has to use the latter.
pub fn apply_manifest_versions(
    nodes: &mut [ap_ecosystem::pip::Node],
    source: &dyn Source,
) -> Result<(), String> {
    for node in nodes.iter_mut() {
        if let Some(version) = latest_version(source, &node.name) {
            node.version = version?;
        }
    }
    Ok(())
}

/// The nodes of `nodes` that are CUDA components get the filesystem package of
/// their release as a requirement, and the package itself is added as a node:
/// the library specs `Require` it, so it has to be built first like any other
/// dependency. Does nothing when the mode is off or no node is a component.
pub fn add_filesystem_nodes(
    nodes: &mut Vec<ap_ecosystem::pip::Node>,
    source: &dyn Source,
) -> Result<(), String> {
    let mut added: Vec<String> = Vec::new();
    for node in nodes.iter_mut() {
        let Some(key) = component(&node.name) else {
            continue;
        };
        if key == FILESYSTEM_KEY {
            continue;
        }
        let release = cuda_release(source, product_for(key))?;
        let fs = crate::nvidia_spec::filesystem_package(&release);
        if !node.requires_dist.contains(&fs) {
            node.requires_dist.push(fs.clone());
        }
        if !added.contains(&fs) {
            added.push(fs);
        }
    }
    for fs in added {
        if nodes.iter().any(|n| n.name == fs) {
            continue;
        }
        let version = latest_version(source, &fs).ok_or_else(|| format!("{fs} is not known"))??;
        nodes.push(ap_ecosystem::pip::Node {
            name: fs,
            version,
            sha256: String::new(),
            requires_dist: Vec::new(),
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        });
    }
    Ok(())
}

pub(super) fn sha256_of(path: &Path) -> std::io::Result<String> {
    crate::pipeline::sha256_file(path)
}

/// Put `url` at `dest` and check it against the manifest's digest. A file
/// already there with that digest is kept (a retry of a package whose archive
/// is half a gigabyte); anything that does not match is removed and refused.
pub(super) fn fetch_verified(
    source: &dyn Source,
    url: &str,
    dest: &Path,
    expected: &str,
) -> Result<(), String> {
    let expected = expected.to_ascii_lowercase();
    if sha256_of(dest).is_ok_and(|have| have == expected) {
        return Ok(());
    }
    let got = source.download(url, dest)?.to_ascii_lowercase();
    if got != expected {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "{url} has sha256 {got}, but NVIDIA's manifest says {expected}: refusing to package it"
        ));
    }
    Ok(())
}
