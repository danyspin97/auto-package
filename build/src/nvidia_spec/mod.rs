//! Spec generation for NVIDIA's CUDA redistributable components.
//!
//! Each component (`cuda_cudart`, `libcublas`, `cuda_nvcc`, `cudnn`, ...) is one
//! `.tar.xz` per platform on `developer.download.nvidia.com/compute/cuda/redist/`.
//! The archives are closed binaries with this shape (checked on cuda_cudart,
//! cuda_nvtx, cuda_crt, cuda_nvdisasm, cuda_nvcc and cccl, 13.4.x):
//!
//! - one top directory `<key>-linux-<arch>-<version>-archive/`;
//! - `LICENSE` at its root (the only top-level file);
//! - `lib/` (NOT `lib64/`) with versioned `libX.so.M.m.p`, the `libX.so.M` and
//!   `libX.so` symlinks, `.a` statics, `lib/stubs/libcuda.so` (cudart),
//!   `lib/cmake/<pkg>/*.cmake` (cccl);
//! - `include/`, `bin/` (executables, plus `nvcc.profile` and `bin/crt/*.stub`),
//!   `pkg-config/*.pc` whose `libdir` points at `targets/x86_64-linux/lib`, a
//!   directory the archive does not have.
//!
//! Nothing here reads the archive: the generator takes the member list.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::NVIDIA_PROPRIETARY;

mod layout;
mod rendering;
#[cfg(test)]
mod tests;

pub use layout::*;
pub use rendering::*;
