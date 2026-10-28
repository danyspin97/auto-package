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
