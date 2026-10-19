//! `--nvidia-redist`: CUDA components packaged from NVIDIA's own redistributable
//! archives (`cuda-cublas`, `cuda-cudnn`...) instead of from the `nvidia-*`
//! PyPI wheels (`python-nvidia-cublas`).
//!
//! The mode is one process-wide switch, set once at startup like the disk
//! guard's minimum: the package name of a dependency is derived in a dozen
//! places that have no config at hand ([`crate::osc::rpm_name`] first of all),
//! and with the switch off every one of them behaves as before.
//!
//! A component is named three ways, and [`component`] maps all of them to the
//! manifest key (`libcublas`): the PyPI wheel (`nvidia-cublas-cu13`), a soname
//! (`libcublas.so.13()(64bit)`) and the package this module names it
//! (`cuda-cublas`). The package version is the component's own manifest
//! version (`13.8.0.4`), which is not the wheel's (`13.1.1.3`): the resolver
//! overwrites the node versions with [`apply_manifest_versions`] so every later
//! comparison of "what version is built" uses the one scheme.
//!
//! Network access goes through [`Source`], so the generation can be run on a
//! manifest and an archive the caller supplies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ap_ecosystem::nvidia_redist::{self, Manifest, Product};

use crate::nvidia_spec::{RedistSpecInput, short_name};

static ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    /// Per-thread value for tests, which run in parallel in one process.
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Turn the mode on or off. Set once at startup from `--nvidia-redist`.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    #[cfg(test)]
    if let Some(v) = TEST_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    ENABLED.load(Ordering::Relaxed)
}

/// Holds the mode at a value on the current thread until dropped.
#[cfg(test)]
pub struct TestMode(Option<bool>);

#[cfg(test)]
impl TestMode {
    pub fn set(on: bool) -> Self {
        TestMode(TEST_OVERRIDE.with(|c| c.replace(Some(on))))
    }
}

#[cfg(test)]
impl Drop for TestMode {
    fn drop(&mut self) {
        TEST_OVERRIDE.with(|c| c.set(self.0));
    }
}

/// The CUDA major the cuDNN archives are picked for (the manifest has one
/// archive per CUDA major).
pub const CUDNN_CUDA_MAJOR: &str = "13";

/// The synthetic component of a package that is no archive of NVIDIA's:
/// `cuda-<major>-<minor>-filesystem`, which owns the `ld.so.conf.d` file of one
/// CUDA release (see [`crate::nvidia_spec::filesystem_package`]).
pub const FILESYSTEM_KEY: &str = "cuda_filesystem";

/// `cuda-13-4-filesystem` -> `"13.4"`; `None` for any other name.
fn filesystem_mm(name: &str) -> Option<String> {
    let mm = name
        .trim()
        .strip_prefix("cuda-")?
        .strip_suffix("-filesystem")?;
    let (maj, min) = mm.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(maj) && digits(min)).then(|| format!("{maj}.{min}"))
}

/// The manifest key of the component `name` stands for, when the mode is on: a
/// PyPI name, a soname or a `cuda-<short>` package name. `None` for everything
/// else, and always when the mode is off.
pub fn component(name: &str) -> Option<&'static str> {
    if !enabled() {
        return None;
    }
    let name = name.trim();
    if filesystem_mm(name).is_some() {
        return Some(FILESYSTEM_KEY);
    }
    nvidia_redist::component_for_package(name).or_else(|| {
        let short = name.strip_prefix("cuda-")?;
        nvidia_redist::COMPONENT_KEYS
            .iter()
            .copied()
            .find(|k| short_name(k) == short)
    })
}

/// `cuda-<short>`, the package for the component `name` stands for.
pub fn package_name(name: &str) -> Option<String> {
    if filesystem_mm(name).is_some() && enabled() {
        return Some(name.trim().to_string());
    }
    component(name).map(|key| format!("cuda-{}", short_name(key)))
}

/// Whether `rpm_name` is a package this mode builds from a redistributable.
pub fn is_redist_package(rpm_name: &str) -> bool {
    rpm_name.starts_with("cuda-") && component(rpm_name).is_some()
}

fn product_for(key: &str) -> Product {
    match key {
        "cudnn" => Product::Cudnn,
        "libcusparse_lt" => Product::Cusparselt,
        "libnvshmem" => Product::Nvshmem,
        _ => Product::Cuda,
    }
}

/// The manifest platform of an RPM architecture.
fn platform_for(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("linux-x86_64"),
        "aarch64" => Ok("linux-sbsa"),
        other => Err(format!(
            "NVIDIA ships no redistributable archive for {other}"
        )),
    }
}

/// What the mode takes from the network.
pub trait Source {
    /// The latest manifest of `product`.
    fn manifest(&self, product: Product) -> Result<Manifest, String>;
    /// Stream `url` to `dest`, returning the SHA-256 of what was written.
    fn download(&self, url: &str, dest: &Path) -> Result<String, String>;
}

/// NVIDIA's servers. Each product's manifest is fetched once per process.
pub struct Network;

static MANIFESTS: OnceLock<Mutex<Vec<(Product, Manifest)>>> = OnceLock::new();

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

fn find_component<'a>(
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
fn filesystem_version(source: &dyn Source, mm: &str) -> Result<String, String> {
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

fn sha256_of(path: &Path) -> std::io::Result<String> {
    crate::pipeline::sha256_file(path)
}

/// Put `url` at `dest` and check it against the manifest's digest. A file
/// already there with that digest is kept (a retry of a package whose archive
/// is half a gigabyte); anything that does not match is removed and refused.
fn fetch_verified(
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

/// The CUDA release whose `major.minor` names the install prefix: the CUDA
/// manifest's own release, and for cuDNN (whose manifest is labelled with
/// cuDNN's version) that of the CUDA the archive was picked for.
fn cuda_release(source: &dyn Source, product: Product) -> Result<String, String> {
    let label = source.manifest(Product::Cuda)?.release_label;
    if product == Product::Cuda || label.split('.').next() == Some(CUDNN_CUDA_MAJOR) {
        return Ok(label);
    }
    Ok(format!("{CUDNN_CUDA_MAJOR}.0"))
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
        rpmlintrc_file: format!("{rpm_name}.rpmlintrc"),
        rpmlintrc,
        archive: dest,
        url,
        sha256: archive.sha256.to_ascii_lowercase(),
    })
}

/// The spec of a `cuda-<major>-<minor>-filesystem` package: no archive, so
/// nothing is downloaded and `archive` names a file that is not there (no stale
/// archive matches it).
fn generate_filesystem(
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
        rpmlintrc_file: format!("{rpm_name}.rpmlintrc"),
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
    // `<key>-linux-<arch>-<version>-archive.tar.xz`: the part before the
    // version identifies the component and platform.
    let Some((stem, _)) = keep_name.split_once("-linux-") else {
        return;
    };
    let prefix = format!("{stem}-linux-");
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::RefCell;

    /// A tar.xz of a cudart-shaped tree.
    pub fn tiny_archive(top: &str) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut raw);
            let mut dir = tar::Header::new_gnu();
            dir.set_entry_type(tar::EntryType::Directory);
            dir.set_size(0);
            dir.set_mode(0o755);
            dir.set_cksum();
            ar.append_data(&mut dir, format!("{top}/lib"), std::io::empty())
                .unwrap();
            for name in [
                "LICENSE",
                "lib/libcudart.so.13.4.92",
                "include/cuda_runtime.h",
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(2);
                h.set_mode(0o644);
                h.set_cksum();
                ar.append_data(&mut h, format!("{top}/{name}"), &b"hi"[..])
                    .unwrap();
            }
            ar.finish().unwrap();
        }
        let mut out = Vec::new();
        let mut w = lzma_rust2::XzWriter::new(&mut out, lzma_rust2::XzOptions::default()).unwrap();
        std::io::Write::write_all(&mut w, &raw).unwrap();
        w.finish().unwrap();
        out
    }

    pub fn hex_sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// The CUDA manifest with `cuda_cudart` 13.4.92 whose x86_64 archive has
    /// `sha`, and the cuDNN one with `cudnn` 9.9.0.52 for CUDA 12 and 13.
    pub fn manifest_json(product: &str, sha: &str) -> String {
        if product == "cudnn" {
            format!(
                r#"{{"release_date":"2026-10-01","release_label":"9.9.0","release_product":"cudnn",
                "cudnn":{{"name":"NVIDIA CUDA Deep Neural Network library","license":"cudnn",
                "license_path":"cudnn/LICENSE.txt","version":"9.9.0.52",
                "linux-x86_64":{{
                  "cuda12":{{"relative_path":"cudnn/linux-x86_64/cudnn-linux-x86_64-9.9.0.52_cuda12-archive.tar.xz","sha256":"{}","md5":"0","size":"1"}},
                  "cuda13":{{"relative_path":"cudnn/linux-x86_64/cudnn-linux-x86_64-9.9.0.52_cuda13-archive.tar.xz","sha256":"{sha}","md5":"0","size":"1"}}}}}}}}"#,
                "0".repeat(64)
            )
        } else {
            format!(
                r#"{{"release_date":"2026-10-13","release_label":"13.4.3","release_product":"cuda",
                "cuda_cudart":{{"name":"CUDA Runtime (cudart)","license":"CUDA Toolkit",
                "license_path":"cuda_cudart/LICENSE.txt","version":"13.4.92",
                "linux-x86_64":{{"relative_path":"cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz","sha256":"{sha}","md5":"0","size":"1"}},
                "linux-sbsa":{{"relative_path":"cuda_cudart/linux-sbsa/cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz","sha256":"{sha}","md5":"0","size":"1"}}}},
                "libcublas":{{"name":"CUDA cuBLAS","license":"CUDA Toolkit",
                "license_path":"libcublas/LICENSE.txt","version":"13.8.0.4",
                "linux-x86_64":{{"relative_path":"libcublas/linux-x86_64/libcublas-linux-x86_64-13.8.0.4-archive.tar.xz","sha256":"{sha}","md5":"0","size":"1"}}}}}}"#
            )
        }
    }

    /// Serves manifests and one archive from memory; counts what was asked.
    pub struct Fake {
        pub archive: Vec<u8>,
        /// The digest the manifests carry (the real one unless a test lies).
        pub advertised: String,
        pub manifests: RefCell<Vec<Product>>,
        pub downloads: RefCell<Vec<String>>,
    }

    impl Fake {
        pub fn new(top: &str) -> Self {
            let archive = tiny_archive(top);
            Fake {
                advertised: hex_sha(&archive),
                archive,
                manifests: Default::default(),
                downloads: Default::default(),
            }
        }
    }

    impl Source for Fake {
        fn manifest(&self, product: Product) -> Result<Manifest, String> {
            self.manifests.borrow_mut().push(product);
            let name = if product == Product::Cudnn {
                "cudnn"
            } else {
                "cuda"
            };
            nvidia_redist::parse(&manifest_json(name, &self.advertised), product.base_url())
                .map_err(|e| e.to_string())
        }

        fn download(&self, url: &str, dest: &Path) -> Result<String, String> {
            self.downloads.borrow_mut().push(url.to_string());
            std::fs::write(dest, &self.archive).map_err(|e| e.to_string())?;
            Ok(hex_sha(&self.archive))
        }
    }

    #[test]
    fn the_mode_is_off_by_default_and_maps_nothing() {
        let _off = TestMode::set(false);
        assert_eq!(component("nvidia-cublas"), None);
        assert_eq!(package_name("libcublas.so.13()(64bit)"), None);
        assert!(!is_redist_package("cuda-cublas"));
    }

    #[test]
    fn a_wheel_a_soname_and_a_package_name_are_the_same_component() {
        let _on = TestMode::set(true);
        for name in [
            "nvidia-cublas",
            "nvidia_cublas_cu13",
            "libcublas.so.13()(64bit)",
            "libcublasLt.so.13",
            "cuda-cublas",
        ] {
            assert_eq!(component(name), Some("libcublas"), "{name}");
            assert_eq!(package_name(name).as_deref(), Some("cuda-cublas"), "{name}");
        }
        assert_eq!(
            package_name("nvidia-cuda-runtime").as_deref(),
            Some("cuda-cudart")
        );
        assert_eq!(
            package_name("nvidia-cudnn-cu13").as_deref(),
            Some("cuda-cudnn")
        );
        assert!(is_redist_package("cuda-cudart"));
    }

    #[test]
    fn the_driver_and_unrelated_names_are_not_components() {
        let _on = TestMode::set(true);
        for name in [
            "libcuda.so.1()(64bit)",
            "numpy",
            "cuda-devel",
            "cuda-toolkit",
            "nvidia-smi",
        ] {
            assert_eq!(component(name), None, "{name}");
        }
        assert!(!is_redist_package("python-nvidia-cublas"));
    }

    #[test]
    fn a_node_takes_the_manifests_version_not_the_wheels() {
        let _on = TestMode::set(true);
        let node = |name: &str, version: &str| ap_ecosystem::pip::Node {
            name: name.to_string(),
            version: version.to_string(),
            sha256: String::new(),
            requires_dist: Vec::new(),
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        };
        let mut nodes = vec![
            node("nvidia-cuda-runtime", "13.1.80"),
            node("numpy", "2.3.0"),
        ];
        let fake = Fake::new("top");
        apply_manifest_versions(&mut nodes, &fake).unwrap();
        assert_eq!(nodes[0].version, "13.4.92");
        assert_eq!(nodes[1].version, "2.3.0");
        assert_eq!(*fake.manifests.borrow(), [Product::Cuda]);
    }

    #[test]
    fn the_filesystem_package_is_a_component_a_node_and_a_spec() {
        let _on = TestMode::set(true);
        assert_eq!(component("cuda-13-4-filesystem"), Some(FILESYSTEM_KEY));
        assert_eq!(
            package_name("cuda-13-4-filesystem").as_deref(),
            Some("cuda-13-4-filesystem")
        );
        assert!(is_redist_package("cuda-13-4-filesystem"));
        for no in [
            "cuda-13-filesystem",
            "cuda-x-4-filesystem",
            "cuda--4-filesystem",
            "cuda-13-4-filesystemx",
        ] {
            assert_eq!(component(no), None, "{no}");
        }
        {
            let _off = TestMode::set(false);
            assert_eq!(package_name("cuda-13-4-filesystem"), None);
        }
        let node = |name: &str| ap_ecosystem::pip::Node {
            name: name.to_string(),
            version: "1".to_string(),
            sha256: String::new(),
            requires_dist: Vec::new(),
            requires_python: None,
            is_requested: false,
            is_yanked: false,
        };
        let fake = Fake::new("top");
        let mut nodes = vec![
            node("nvidia-cuda-runtime"),
            node("nvidia-cublas"),
            node("numpy"),
        ];
        add_filesystem_nodes(&mut nodes, &fake).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].requires_dist, ["cuda-13-4-filesystem"]);
        assert_eq!(nodes[1].requires_dist, ["cuda-13-4-filesystem"]);
        assert!(nodes[2].requires_dist.is_empty());
        assert_eq!(
            (nodes[3].name.as_str(), nodes[3].version.as_str()),
            ("cuda-13-4-filesystem", "13.4.3")
        );
        // Once: a second pass adds neither the requirement nor the node.
        add_filesystem_nodes(&mut nodes, &fake).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].requires_dist.len(), 1);
        let mut none = vec![node("numpy")];
        add_filesystem_nodes(&mut none, &fake).unwrap();
        assert_eq!(none.len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let g = generate(
            &fake,
            "cuda-13-4-filesystem",
            "13.4.3",
            "x86_64",
            dir.path(),
        )
        .unwrap();
        assert!(
            g.spec.contains("Name:           cuda-13-4-filesystem\n"),
            "{}",
            g.spec
        );
        assert!(fake.downloads.borrow().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(
            generate(
                &fake,
                "cuda-13-4-filesystem",
                "13.4.9",
                "x86_64",
                dir.path()
            )
            .is_err()
        );
        // The stale-archive sweep keeps its hands off the checkout.
        std::fs::write(
            dir.path().join("cuda_cudart-linux-x86_64-1-archive.tar.xz"),
            "x",
        )
        .unwrap();
        remove_stale_archives(dir.path(), &g.archive);
        assert!(
            dir.path()
                .join("cuda_cudart-linux-x86_64-1-archive.tar.xz")
                .exists()
        );
    }

    #[test]
    fn a_component_missing_from_the_manifest_is_an_error() {
        let _on = TestMode::set(true);
        let fake = Fake::new("top");
        // cufft is mapped, but the fixture manifest does not list it.
        let err = latest_version(&fake, "nvidia-cufft").unwrap().unwrap_err();
        assert!(err.contains("libcufft"), "{err}");
        assert!(latest_version(&fake, "numpy").is_none());
    }

    #[test]
    fn generate_downloads_verifies_lists_and_writes_the_spec_inputs() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let top = "cuda_cudart-linux-x86_64-13.4.92-archive";
        let fake = Fake::new(top);
        let g = generate(&fake, "cuda-cudart", "13.4.92", "x86_64", dir.path()).unwrap();
        assert_eq!(
            g.archive.file_name().unwrap().to_str().unwrap(),
            format!("{top}.tar.xz")
        );
        assert!(g.archive.is_file());
        assert_eq!(g.sha256, fake.advertised);
        assert_eq!(
            g.url,
            format!(
                "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/linux-x86_64/{top}.tar.xz"
            )
        );
        assert!(
            g.spec.contains("Name:           cuda-cudart\n"),
            "{}",
            g.spec
        );
        assert!(g.spec.contains("Version:        13.4.92\n"));
        assert!(g.spec.contains("%global cuda_mm 13.4\n"));
        assert!(g.spec.contains(&format!("%setup -q -n {top}\n")));
        assert!(g.spec.contains("%{cuda_prefix}/lib64/libcudart.so.13.4.92"));
        assert!(g.spec.contains("Source99:       cuda-cudart.rpmlintrc\n"));
        assert!(
            g.spec
                .contains(&format!("# Source archive sha256: {}\n", g.sha256))
        );
        assert!(
            g.spec
                .contains("# Source archive: https://developer.download.nvidia.com/")
        );
        assert_eq!(g.rpmlintrc_file, "cuda-cudart.rpmlintrc");
        assert!(g.rpmlintrc.contains("filelist-forbidden-fhs23"));
        // No Python in it.
        assert!(!g.spec.contains("%pyproject") && !g.spec.contains("python_expand"));
    }

    #[test]
    fn aarch64_takes_the_sbsa_archive() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new("cuda_cudart-linux-sbsa-13.4.92-archive");
        let g = generate(&fake, "cuda-cudart", "13.4.92", "aarch64", dir.path()).unwrap();
        assert!(
            g.url
                .ends_with("cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz"),
            "{}",
            g.url
        );
        assert!(g.spec.contains("ExclusiveArch:  aarch64"));
        // The fixture has no sbsa archive for cublas.
        let err = generate(&fake, "cuda-cublas", "13.8.0.4", "aarch64", dir.path()).unwrap_err();
        assert!(err.contains("no linux-sbsa archive"), "{err}");
    }

    #[test]
    fn a_digest_that_does_not_match_the_manifest_is_refused_and_removed() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::new("cuda_cudart-linux-x86_64-13.4.92-archive");
        fake.advertised = "a".repeat(64);
        let err = generate(&fake, "cuda-cudart", "13.4.92", "x86_64", dir.path()).unwrap_err();
        assert!(err.contains("refusing to package it"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_archive_already_in_the_checkout_is_not_downloaded_again() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new("cuda_cudart-linux-x86_64-13.4.92-archive");
        generate(&fake, "cuda-cudart", "13.4.92", "x86_64", dir.path()).unwrap();
        generate(&fake, "cuda-cudart", "13.4.92", "x86_64", dir.path()).unwrap();
        assert_eq!(fake.downloads.borrow().len(), 1);
    }

    #[test]
    fn a_version_that_is_not_the_manifests_is_refused() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new("top");
        // The PyPI wheel's version.
        let err = generate(&fake, "cuda-cudart", "13.1.80", "x86_64", dir.path()).unwrap_err();
        assert!(err.contains("13.4.92"), "{err}");
        assert!(fake.downloads.borrow().is_empty());
    }

    #[test]
    fn cudnn_picks_the_archive_of_the_cuda_major_and_the_cuda_prefix() {
        let _on = TestMode::set(true);
        let dir = tempfile::tempdir().unwrap();
        let top = "cudnn-linux-x86_64-9.9.0.52_cuda13-archive";
        let fake = Fake::new(top);
        let g = generate(&fake, "cuda-cudnn", "9.9.0.52", "x86_64", dir.path()).unwrap();
        assert!(g.url.ends_with("_cuda13-archive.tar.xz"), "{}", g.url);
        assert!(g.url.contains("/compute/cudnn/redist/"));
        assert!(g.spec.contains("Version:        9.9.0.52\n"));
        // The prefix is CUDA's (13.4), not cuDNN's version (9.9).
        assert!(g.spec.contains("%global cuda_mm 13.4\n"), "{}", g.spec);
        assert_eq!(*fake.manifests.borrow(), [Product::Cudnn, Product::Cuda]);
    }

    #[test]
    fn the_archives_of_another_release_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "cuda_cudart-linux-x86_64-13.3.0-archive.tar.xz",
            "cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz",
            "cuda_nvtx-linux-x86_64-13.4.0-archive.tar.xz",
            "cuda-cudart.spec",
        ] {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        let keep = dir
            .path()
            .join("cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz");
        remove_stale_archives(dir.path(), &keep);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "cuda-cudart.spec",
                "cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz",
                "cuda_nvtx-linux-x86_64-13.4.0-archive.tar.xz"
            ]
        );
    }

    #[test]
    fn cusparselt_and_nvshmem_are_components_with_their_own_manifests() {
        let _on = TestMode::set(true);
        for (name, key, pkg) in [
            (
                "nvidia-cusparselt-cu13",
                "libcusparse_lt",
                "cuda-cusparse-lt",
            ),
            ("nvidia_nvshmem_cu13", "libnvshmem", "cuda-nvshmem"),
            (
                "libcusparseLt.so.0()(64bit)",
                "libcusparse_lt",
                "cuda-cusparse-lt",
            ),
            ("libnvshmem_host.so.3", "libnvshmem", "cuda-nvshmem"),
            ("cuda-cusparse-lt", "libcusparse_lt", "cuda-cusparse-lt"),
            ("cuda-nvshmem", "libnvshmem", "cuda-nvshmem"),
        ] {
            assert_eq!(component(name), Some(key), "{name}");
            assert_eq!(package_name(name).as_deref(), Some(pkg), "{name}");
        }
        assert_eq!(product_for("libnvshmem"), Product::Nvshmem);
        assert_eq!(product_for("libcusparse_lt"), Product::Cusparselt);
        assert_eq!(product_for("libcublas"), Product::Cuda);
        assert!(
            Product::Nvshmem
                .base_url()
                .ends_with("/compute/nvshmem/redist")
        );
        assert!(
            Product::Cusparselt
                .base_url()
                .ends_with("/compute/cusparselt/redist")
        );
        assert!(Product::Nvshmem.per_cuda_major() && !Product::Cuda.per_cuda_major());
    }
}
