//! NVIDIA redistributable manifests (CUDA toolkit components, cuDNN, cuSPARSELt and NVSHMEM).
//!
//! NVIDIA publishes `redistrib_<ver>.json` next to the archives at
//! `https://developer.download.nvidia.com/compute/{cuda,cudnn,cusparselt,nvshmem}/redist/`. Each
//! manifest has `release_date`, `release_label` and `release_product`, then
//! one object per component (`name`, `license`, `license_path`, `version`) with
//! one entry per platform (`linux-x86_64`, `linux-sbsa`, `windows-x86_64`...).
//! A platform entry is either `{relative_path, sha256, md5, size}` or, in the
//! cuDNN manifests, an object keyed by CUDA major (`cuda12`) holding those.
//! `size` is a string. Other keys (such as cuDNN's `cuda_variant` list) are
//! ignored.

use std::time::Duration;

use serde_json::Value;

const CUDA_BASE: &str = "https://developer.download.nvidia.com/compute/cuda/redist";
const CUDNN_BASE: &str = "https://developer.download.nvidia.com/compute/cudnn/redist";
const CUSPARSELT_BASE: &str = "https://developer.download.nvidia.com/compute/cusparselt/redist";
const NVSHMEM_BASE: &str = "https://developer.download.nvidia.com/compute/nvshmem/redist";
const MAX_MANIFEST_BYTES: u64 = 16 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub product: String,
    pub release_label: String,
    pub base_url: String,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// The JSON key, e.g. `cuda_cudart`.
    pub key: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub license_path: String,
    pub archives: Vec<Archive>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub platform: String,
    /// CUDA major (`"12"`) for the nested per-CUDA shape, else `None`.
    pub cuda_variant: Option<String>,
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
}

impl Component {
    /// The archive for `platform`. With `cuda_major` (`"13"` or `"cuda13"`)
    /// only that variant matches; without it a non-variant archive is
    /// preferred, else the newest variant.
    pub fn archive_for(&self, platform: &str, cuda_major: Option<&str>) -> Option<&Archive> {
        let mut it = self.archives.iter().filter(|a| a.platform == platform);
        match cuda_major {
            Some(want) => {
                let want = want.strip_prefix("cuda").unwrap_or(want);
                it.find(|a| a.cuda_variant.as_deref() == Some(want))
            }
            None => {
                let all: Vec<&Archive> = it.collect();
                all.iter()
                    .find(|a| a.cuda_variant.is_none())
                    .or_else(|| {
                        all.iter()
                            .max_by_key(|a| a.cuda_variant.as_deref().and_then(|v| v.parse::<u64>().ok()))
                    })
                    .copied()
            }
        }
    }
}

impl Archive {
    pub fn url(&self, base: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), self.relative_path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Cuda,
    Cudnn,
    /// cuSPARSELt (`libcusparse_lt`), one archive per CUDA major.
    Cusparselt,
    /// NVSHMEM (`libnvshmem`), one archive per CUDA major.
    Nvshmem,
}

impl Product {
    pub fn base_url(self) -> &'static str {
        match self {
            Product::Cuda => CUDA_BASE,
            Product::Cudnn => CUDNN_BASE,
            Product::Cusparselt => CUSPARSELT_BASE,
            Product::Nvshmem => NVSHMEM_BASE,
        }
    }

    /// Whether the product's archives are split by CUDA major (`cuda13`), so
    /// that one has to be picked. The CUDA toolkit's are not.
    pub fn per_cuda_major(self) -> bool {
        self != Product::Cuda
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid redistributable manifest: {0}")]
    Json(#[from] serde_json::Error),

    #[error("manifest field {0} is missing or malformed")]
    Field(String),

    #[error("no redistrib_*.json in the directory listing of {0}")]
    NoManifest(String),

    #[error("fetching {url}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
}

fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_string)
}

fn parse_archive(v: &Value, platform: &str, variant: Option<&str>, ctx: &str) -> Result<Archive, Error> {
    let bad = |f: &str| Error::Field(format!("{ctx}.{platform}.{f}"));
    let size = match v.get("size") {
        Some(Value::String(s)) => s.parse().map_err(|_| bad("size"))?,
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| bad("size"))?,
        _ => return Err(bad("size")),
    };
    Ok(Archive {
        platform: platform.to_string(),
        cuda_variant: variant.map(|v| v.strip_prefix("cuda").unwrap_or(v).to_string()),
        relative_path: str_field(v, "relative_path").ok_or_else(|| bad("relative_path"))?,
        sha256: str_field(v, "sha256").ok_or_else(|| bad("sha256"))?,
        size,
    })
}

/// Parse a manifest. `base_url` is where `relative_path`s are rooted.
pub fn parse(json: &str, base_url: &str) -> Result<Manifest, Error> {
    let root: Value = serde_json::from_str(json)?;
    let obj = root.as_object().ok_or_else(|| Error::Field("<root>".into()))?;
    let mut components = Vec::new();
    for (key, comp) in obj {
        // Components are the objects that carry a name and a version.
        let (Some(name), Some(version)) = (str_field(comp, "name"), str_field(comp, "version")) else {
            continue;
        };
        let mut archives = Vec::new();
        for (platform, entry) in comp.as_object().into_iter().flatten() {
            if !entry.is_object() {
                continue;
            }
            if entry.get("relative_path").is_some() {
                archives.push(parse_archive(entry, platform, None, key)?);
            } else {
                for (variant, inner) in entry.as_object().into_iter().flatten() {
                    if inner.get("relative_path").is_some() {
                        archives.push(parse_archive(inner, platform, Some(variant), key)?);
                    }
                }
            }
        }
        components.push(Component {
            key: key.clone(),
            name,
            version,
            license: str_field(comp, "license").unwrap_or_default(),
            license_path: str_field(comp, "license_path").unwrap_or_default(),
            archives,
        });
    }
    Ok(Manifest {
        product: str_field(&root, "release_product").ok_or_else(|| Error::Field("release_product".into()))?,
        release_label: str_field(&root, "release_label").ok_or_else(|| Error::Field("release_label".into()))?,
        base_url: base_url.trim_end_matches('/').to_string(),
        components,
    })
}

fn version_key(name: &str) -> Option<Vec<u64>> {
    let v = name.strip_prefix("redistrib_")?.strip_suffix(".json")?;
    v.split('.').map(|p| p.parse().ok()).collect()
}

/// The highest `redistrib_X.Y.Z.json` named in a directory listing.
pub fn latest_manifest_name(index_html: &str) -> Option<String> {
    index_html
        .split("redistrib_")
        .skip(1)
        .filter_map(|rest| {
            let end = rest.find(".json")? + ".json".len();
            let name = format!("redistrib_{}", &rest[..end]);
            Some((version_key(&name)?, name))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, n)| n)
}

fn get(url: &str) -> Result<String, Error> {
    let err = |e| Error::Http { url: url.to_string(), source: Box::new(e) };
    let agent = crate::net::agent(ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)));
    crate::net::call_with_retries(|| agent.get(url))
        .map_err(err)?
        .into_body()
        .with_config()
        .limit(MAX_MANIFEST_BYTES)
        .read_to_string()
        .map_err(err)
}

/// Fetch a manifest: the pinned `version` (`"13.4.3"`), else the latest listed.
pub fn fetch_manifest(product: Product, version: Option<&str>) -> Result<Manifest, Error> {
    let base = product.base_url();
    let name = match version {
        Some(v) => format!("redistrib_{v}.json"),
        None => latest_manifest_name(&get(&format!("{base}/"))?)
            .ok_or_else(|| Error::NoManifest(base.to_string()))?,
    };
    parse(&get(&format!("{base}/{name}"))?, base)
}

/// Manifest component key for a PyPI name or a soname/RPM dependency. `None`
/// for anything unknown, and for `libcuda` (the driver, not redistributable).
pub fn component_for_package(name: &str) -> Option<&'static str> {
    let n = name.trim();
    let n = n.split('(').next().unwrap_or(n);
    if let Some(rest) = n.strip_prefix("lib") {
        let stem = rest.split(".so").next()?;
        if stem.len() == rest.len() {
            return None;
        }
        return soname_component(stem);
    }
    pypi_component(n)
}

/// Every component key [`component_for_package`] can return, so a package name
/// built from one (`cuda-cublas`) can be mapped back.
pub const COMPONENT_KEYS: &[&str] = &[
    "libcublas",
    "cuda_cudart",
    "cuda_nvrtc",
    "libcufft",
    "libcurand",
    "libcusolver",
    "libcusparse",
    "libnvjitlink",
    "libnvfatbin",
    "libnvvm",
    "libcufile",
    "cuda_cupti",
    "cuda_nvtx",
    "libnvjpeg",
    "cudnn",
    "libcusparse_lt",
    "libnvshmem",
    "libnpp",
    "cuda_nvcc",
    "cuda_crt",
    "cuda_nvdisasm",
    "cccl",
];

fn soname_component(stem: &str) -> Option<&'static str> {
    let s = stem.to_ascii_lowercase();
    Some(match s.as_str() {
        "cublas" | "cublaslt" | "nvblas" => "libcublas",
        "cudart" => "cuda_cudart",
        "nvrtc" | "nvrtc-builtins" => "cuda_nvrtc",
        "cufft" | "cufftw" => "libcufft",
        "curand" => "libcurand",
        "cusolver" | "cusolvermg" => "libcusolver",
        "cusparse" => "libcusparse",
        "nvjitlink" => "libnvjitlink",
        "nvfatbin" => "libnvfatbin",
        "nvvm" => "libnvvm",
        "cufile" => "libcufile",
        "cupti" => "cuda_cupti",
        "nvtoolsext" => "cuda_nvtx",
        "nvjpeg" => "libnvjpeg",
        "cudnn" => "cudnn",
        "cusparselt" => "libcusparse_lt",
        s if s.starts_with("nvshmem") => "libnvshmem",
        s if s.starts_with("cudnn_") => "cudnn",
        s if s.starts_with("npp") => "libnpp",
        _ => return None,
    })
}

fn pypi_component(name: &str) -> Option<&'static str> {
    let mut n = name.to_ascii_lowercase().replace('_', "-");
    // `-cu12` / `-cu13` suffix selects the CUDA major, not the component.
    if let Some((head, tail)) = n.rsplit_once('-')
        && tail.len() > 2
        && tail.starts_with("cu")
        && tail[2..].bytes().all(|b| b.is_ascii_digit())
    {
        n = head.to_string();
    }
    Some(match n.as_str() {
        "nvidia-cublas" => "libcublas",
        "nvidia-cuda-runtime" => "cuda_cudart",
        "nvidia-cuda-nvrtc" => "cuda_nvrtc",
        "nvidia-cufft" => "libcufft",
        "nvidia-curand" => "libcurand",
        "nvidia-cusolver" => "libcusolver",
        "nvidia-cusparse" => "libcusparse",
        "nvidia-nvjitlink" => "libnvjitlink",
        "nvidia-nvtx" => "cuda_nvtx",
        "nvidia-cuda-cupti" => "cuda_cupti",
        "nvidia-cuda-nvcc" => "cuda_nvcc",
        "nvidia-cuda-crt" => "cuda_crt",
        "nvidia-cuda-nvdisasm" => "cuda_nvdisasm",
        "nvidia-cuda-cccl" => "cccl",
        "nvidia-cufile" => "libcufile",
        "nvidia-nvfatbin" => "libnvfatbin",
        "nvidia-npp" => "libnpp",
        "nvidia-nvjpeg" => "libnvjpeg",
        "nvidia-cudnn" => "cudnn",
        "nvidia-cusparselt" => "libcusparse_lt",
        "nvidia-nvshmem" => "libnvshmem",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real redistrib_13.4.3.json and redistrib_9.9.0.json
    /// (merged into one document; sizes and hashes are the real ones).
    const FIXTURE: &str = r#"{
    "release_date": "2026-10-13",
    "release_label": "13.4.3",
    "release_product": "cuda",
    "cuda_cudart": {
        "name": "CUDA Runtime (cudart)",
        "license": "CUDA Toolkit",
        "license_path": "cuda_cudart/LICENSE.txt",
        "version": "13.4.92",
        "linux-x86_64": {
            "relative_path": "cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz",
            "sha256": "0ac5dbc538d04e9983bc493b410cce4b459e1ee9f5f6654b6464ef7b3e14a8b5",
            "md5": "69274031cdeedd6ba644d20555c2e823",
            "size": "1704640"
        },
        "linux-sbsa": {
            "relative_path": "cuda_cudart/linux-sbsa/cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz",
            "sha256": "47e65e60d0f7cf832bff7992bd7a87ba4888195c31210392b5f23c607fe6c2a8",
            "md5": "cdf0b4c99804cefc3d6c92961c76effe",
            "size": "1694356"
        }
    },
    "libcublas": {
        "name": "CUDA cuBLAS",
        "license": "CUDA Toolkit",
        "license_path": "libcublas/LICENSE.txt",
        "version": "13.8.0.4",
        "linux-x86_64": {
            "relative_path": "libcublas/linux-x86_64/libcublas-linux-x86_64-13.8.0.4-archive.tar.xz",
            "sha256": "bd6ffcce561672da6abb928214a6694f392741e7d1e2d7dfde0da4bd651706b6",
            "md5": "20226011cceb7df64eecaf890bb26a7d",
            "size": "877123136"
        }
    },
    "cuda_nvcc": {
        "name": "Cuda Nvcc",
        "license": "CUDA Toolkit",
        "license_path": "cuda_nvcc/LICENSE.txt",
        "version": "13.4.92",
        "linux-x86_64": {
            "relative_path": "cuda_nvcc/linux-x86_64/cuda_nvcc-linux-x86_64-13.4.92-archive.tar.xz",
            "sha256": "60998f40cc9df5826b2a846e2dcc328133fbc5ef539398ce0a21cf5dcac7193d",
            "md5": "745470e5587ac8040942d5d6df8749a2",
            "size": "33304524"
        }
    },
    "cudnn": {
        "name": "NVIDIA CUDA Deep Neural Network library",
        "license": "cudnn",
        "license_path": "cudnn/LICENSE.txt",
        "version": "9.9.0.52",
        "linux-x86_64": {
            "cuda11": {
                "relative_path": "cudnn/linux-x86_64/cudnn-linux-x86_64-9.9.0.52_cuda11-archive.tar.xz",
                "sha256": "5696665117d87f112024ec47d7d3a90f5f338d13226dd6ea50e5b24d6163872f",
                "md5": "8d5716f333e0366233477842cdbbde4d",
                "size": "561564084"
            },
            "cuda12": {
                "relative_path": "cudnn/linux-x86_64/cudnn-linux-x86_64-9.9.0.52_cuda12-archive.tar.xz",
                "sha256": "73f99e9c5d39cb7799dfe0f7b42d09a0bf6e3f5a90dbd16d570438daa1daea18",
                "md5": "a0d9d43997565426e3a99410ef37179b",
                "size": "1093260928"
            }
        },
        "cuda_variant": ["11", "12"]
    }
}"#;

    #[test]
    fn every_mapped_component_is_listed() {
        for name in [
            "nvidia-cublas",
            "nvidia-cuda-runtime",
            "nvidia-cuda-nvrtc",
            "nvidia-cufft",
            "nvidia-curand",
            "nvidia-cusolver",
            "nvidia-cusparse",
            "nvidia-nvjitlink",
            "nvidia-nvtx",
            "nvidia-cuda-cupti",
            "nvidia-cuda-nvcc",
            "nvidia-cuda-crt",
            "nvidia-cuda-nvdisasm",
            "nvidia-cuda-cccl",
            "nvidia-cufile",
            "nvidia-nvfatbin",
            "nvidia-npp",
            "nvidia-nvjpeg",
            "nvidia-cudnn",
            "nvidia-cusparselt-cu13",
            "nvidia-nvshmem-cu13",
            "libnvvm.so.4",
        ] {
            let key = component_for_package(name).unwrap();
            assert!(COMPONENT_KEYS.contains(&key), "{name} -> {key}");
        }
    }

    #[test]
    fn parses_flat_and_nested() {
        let m = parse(FIXTURE, "https://x/redist/").unwrap();
        assert_eq!((m.product.as_str(), m.release_label.as_str()), ("cuda", "13.4.3"));
        assert_eq!(m.base_url, "https://x/redist");
        assert_eq!(m.components.len(), 4);
        let c = m.components.iter().find(|c| c.key == "cuda_cudart").unwrap();
        assert_eq!(c.version, "13.4.92");
        assert_eq!(c.license, "CUDA Toolkit");
        assert_eq!(c.archives.len(), 2);
        let a = c.archive_for("linux-sbsa", None).unwrap();
        assert_eq!(a.size, 1694356);
        assert_eq!(a.cuda_variant, None);
        assert_eq!(
            a.url(&m.base_url),
            "https://x/redist/cuda_cudart/linux-sbsa/cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz"
        );
        assert!(c.archive_for("windows-x86_64", None).is_none());

        let cudnn = m.components.iter().find(|c| c.key == "cudnn").unwrap();
        assert_eq!(cudnn.archives.len(), 2);
        let a12 = cudnn.archive_for("linux-x86_64", Some("12")).unwrap();
        assert!(a12.relative_path.contains("_cuda12-"));
        assert_eq!(cudnn.archive_for("linux-x86_64", Some("cuda11")).unwrap().cuda_variant.as_deref(), Some("11"));
        assert!(cudnn.archive_for("linux-x86_64", Some("13")).is_none());
        // No major requested: newest variant.
        assert_eq!(cudnn.archive_for("linux-x86_64", None).unwrap().cuda_variant.as_deref(), Some("12"));
    }

    #[test]
    fn rejects_bad_manifest() {
        assert!(parse("[]", "b").is_err());
        assert!(parse(r#"{"release_label":"1"}"#, "b").is_err());
        let bad = r#"{"release_label":"1","release_product":"cuda","x":{"name":"n","version":"1",
            "linux-x86_64":{"relative_path":"p","sha256":"s","size":"big"}}}"#;
        assert!(parse(bad, "b").is_err());
    }

    #[test]
    fn latest_is_version_aware() {
        let html = "<a href='redistrib_13.9.1.json'>x</a><a href='redistrib_13.10.0.json'>x</a>\
                    <a href='redistrib_9.99.json'>x</a><a href='redistrib_13.4.3.json'>x</a>\
                    <a href='cuda_cudart/'>c</a>";
        assert_eq!(latest_manifest_name(html).as_deref(), Some("redistrib_13.10.0.json"));
        assert_eq!(latest_manifest_name("<a href='cuda/'>"), None);
    }

    #[test]
    fn package_mapping() {
        for (input, want) in [
            ("nvidia-cublas", "libcublas"),
            ("nvidia-cuda-runtime", "cuda_cudart"),
            ("nvidia-cuda-nvrtc", "cuda_nvrtc"),
            ("nvidia-cufft", "libcufft"),
            ("nvidia-curand", "libcurand"),
            ("nvidia-cusolver", "libcusolver"),
            ("nvidia-cusparse", "libcusparse"),
            ("nvidia-nvjitlink", "libnvjitlink"),
            ("nvidia-nvtx", "cuda_nvtx"),
            ("nvidia-cuda-cupti", "cuda_cupti"),
            ("nvidia-cuda-nvcc", "cuda_nvcc"),
            ("nvidia-cuda-crt", "cuda_crt"),
            ("nvidia-cuda-nvdisasm", "cuda_nvdisasm"),
            ("nvidia-cuda-cccl", "cccl"),
            ("nvidia-cufile", "libcufile"),
            ("nvidia-cudnn-cu13", "cudnn"),
            ("Nvidia_CuDNN_cu12", "cudnn"),
            ("libcublas.so.13()(64bit)", "libcublas"),
            ("libcublasLt.so.13", "libcublas"),
            ("libcudart.so.13", "cuda_cudart"),
            ("libnvrtc.so.13", "cuda_nvrtc"),
            ("libcufft.so.12", "libcufft"),
            ("libcurand.so.10", "libcurand"),
            ("libcusolver.so.12", "libcusolver"),
            ("libcusparse.so.12", "libcusparse"),
            ("libnvJitLink.so.13", "libnvjitlink"),
            ("libnvvm.so.4", "libnvvm"),
            ("libcufile.so.0", "libcufile"),
            ("libcudnn.so.9", "cudnn"),
            ("libcudnn_graph.so.9()(64bit)", "cudnn"),
            ("libnppc.so.13", "libnpp"),
            ("nvidia-cusparselt-cu13", "libcusparse_lt"),
            ("nvidia-nvshmem-cu13", "libnvshmem"),
            ("libcusparseLt.so.0", "libcusparse_lt"),
            ("libnvshmem_host.so.3", "libnvshmem"),
        ] {
            assert_eq!(component_for_package(input), Some(want), "{input}");
        }
        for input in ["libcuda.so.1", "libcuda.so.1()(64bit)", "libc.so.6", "numpy", "nvidia-foo", "libcublas", ""] {
            assert_eq!(component_for_package(input), None, "{input}");
        }
    }

    /// Live check of the manifests `--nvidia-redist` reads for cuSPARSELt and
    /// NVSHMEM: public GETs only. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "network: GETs developer.download.nvidia.com"]
    fn live_cusparselt_and_nvshmem_manifests_parse() {
        for (product, key) in [
            (Product::Cusparselt, "libcusparse_lt"),
            (Product::Nvshmem, "libnvshmem"),
        ] {
            let m = fetch_manifest(product, None).unwrap_or_else(|e| panic!("{product:?}: {e}"));
            let c = m
                .components
                .iter()
                .find(|c| c.key == key)
                .unwrap_or_else(|| panic!("{product:?} has no {key}: {:?}", m.components.iter().map(|c| &c.key).collect::<Vec<_>>()));
            assert!(!c.version.is_empty());
            let a = c
                .archive_for("linux-x86_64", None)
                .unwrap_or_else(|| panic!("{key} has no linux-x86_64 archive"));
            assert!(a.url(&m.base_url).starts_with(product.base_url()));
            assert_eq!(a.sha256.len(), 64, "{key}");
            assert!(a.size > 1 << 20, "{key} archive is {} bytes", a.size);
            // The per-CUDA-major archives the generator picks between.
            assert!(
                c.archive_for("linux-x86_64", Some("13")).is_some()
                    || c.archive_for("linux-x86_64", Some("12")).is_some(),
                "{key}: no cuda12/cuda13 variant"
            );
        }
    }

    #[test]
    #[ignore = "network: GETs developer.download.nvidia.com"]
    fn live_the_listing_names_a_latest_manifest_that_exists() {
        let base = Product::Cusparselt.base_url();
        let name = latest_manifest_name(&get(&format!("{base}/")).unwrap()).unwrap();
        let m = parse(&get(&format!("{base}/{name}")).unwrap(), base).unwrap();
        assert!(!m.components.is_empty());
    }
}
