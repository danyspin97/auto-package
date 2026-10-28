use super::*;

/// The CUDA major the cuDNN archives are picked for (the manifest has one
/// archive per CUDA major).
pub const CUDNN_CUDA_MAJOR: &str = "13";

/// The synthetic component of a package that is no archive of NVIDIA's:
/// `cuda-<major>-<minor>-filesystem`, which owns the `ld.so.conf.d` file of one
/// CUDA release (see [`crate::nvidia_spec::filesystem_package`]).
pub const FILESYSTEM_KEY: &str = "cuda_filesystem";

/// `cuda-13-4-filesystem` -> `"13.4"`; `None` for any other name.
pub(super) fn filesystem_mm(name: &str) -> Option<String> {
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

pub(super) fn product_for(key: &str) -> Product {
    match key {
        "cudnn" => Product::Cudnn,
        "libcusparse_lt" => Product::Cusparselt,
        "libnvshmem" => Product::Nvshmem,
        _ => Product::Cuda,
    }
}

/// The manifest platform of an RPM architecture.
pub(super) fn platform_for(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("linux-x86_64"),
        "aarch64" => Ok("linux-sbsa"),
        other => Err(format!(
            "NVIDIA ships no redistributable archive for {other}"
        )),
    }
}

/// The CUDA release whose `major.minor` names the install prefix: the CUDA
/// manifest's own release, and for cuDNN (whose manifest is labelled with
/// cuDNN's version) that of the CUDA the archive was picked for.
pub(super) fn cuda_release(source: &dyn Source, product: Product) -> Result<String, String> {
    let label = source.manifest(Product::Cuda)?.release_label;
    if product == Product::Cuda || label.split('.').next() == Some(CUDNN_CUDA_MAJOR) {
        return Ok(label);
    }
    Ok(format!("{CUDNN_CUDA_MAJOR}.0"))
}
