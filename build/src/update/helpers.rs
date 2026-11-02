use super::*;

pub(super) const ATTRS_SPEC: &str = "\
Name:           python-attrs
Version:        26.1.0
Source:         https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz
";

pub(super) const NVIDIA_CUBLAS_SPEC: &str = "\
Name:           python-nvidia-cublas
Version:        13.1.1.3
Source0:        nvidia_cublas-13.1.1.3-py3-none-manylinux_2_27_x86_64.whl
";

pub(super) const TORCHAUDIO_SPEC: &str = "\
Name:           python-torchaudio
Version:        2.4.0
Source:         torchaudio-%{version}.tar.gz
";

pub(super) const RIPGREP_SPEC: &str = "\
Name:           ripgrep
Version:        14.1.0
Source:         https://github.com/BurntSushi/ripgrep/archive/14.1.0.tar.gz
";

/// Lookups that find nothing, to fill the ones a test does not exercise.
pub(super) fn unused_lookups() -> Lookups<'static> {
    fn none(_: &str) -> Result<String, String> {
        Err("not looked up in this test".to_string())
    }
    fn no_service(_: &str) -> Option<String> {
        None
    }
    fn has_service(_: &str) -> bool {
        true
    }
    fn release_date(_: &str) -> Result<(String, String), String> {
        Err("not looked up in this test".to_string())
    }
    fn none_in_series(_: &str, _: &[u64]) -> Result<String, String> {
        Err("not looked up in this test".to_string())
    }
    Lookups {
        fetch_pypi_latest: &none,
        fetch_repo_latest: &none,
        fetch_index_latest: &none,
        fetch_repology_latest: &none,
        fetch_gem_latest: &none,
        fetch_cpan_latest: &none,
        fetch_service: &no_service,
        fetch_has_service: &has_service,
        fetch_release_date: &release_date,
        fetch_repo_in_series: &none_in_series,
    }
}

pub(super) fn only_pypi(latest: &'static str) -> Lookups<'static> {
    let p: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_name: &str| Ok(latest.to_string())));
    let r: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_url: &str| -> Result<String, String> {
            unreachable!("repo path not used")
        }));
    Lookups {
        fetch_pypi_latest: p,
        fetch_repo_latest: r,
        ..unused_lookups()
    }
}

pub(super) fn only_repo(latest: &'static str) -> Lookups<'static> {
    let p: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_name: &str| -> Result<String, String> {
            unreachable!("pypi path not used")
        }));
    let r: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_url: &str| Ok(latest.to_string())));
    Lookups {
        fetch_pypi_latest: p,
        fetch_repo_latest: r,
        ..unused_lookups()
    }
}

pub(super) fn lookups_for(
    index: &'static dyn Fn(&str) -> Result<String, String>,
    repology: &'static dyn Fn(&str) -> Result<String, String>,
) -> Lookups<'static> {
    Lookups {
        fetch_index_latest: index,
        fetch_repology_latest: repology,
        ..unused_lookups()
    }
}
