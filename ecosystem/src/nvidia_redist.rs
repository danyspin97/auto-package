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

mod components;
mod fetch;
mod manifest;

#[cfg(test)]
mod tests;

pub use components::*;
pub use fetch::*;
pub use manifest::*;
