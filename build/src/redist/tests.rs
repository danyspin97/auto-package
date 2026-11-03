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
    // Every filter has its reason on the line above.
    let rc: Vec<&str> = g.rpmlintrc.lines().collect();
    for (i, l) in rc
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("addFilter("))
    {
        assert!(rc[i - 1].starts_with("# reason: "), "no reason above {l}");
    }
    // No Python in it.
    assert!(!g.spec.contains("%pyproject") && !g.spec.contains("python_expand"));
}

/// A tar.xz with exactly these members (`(name, is_dir)`).
fn archive_of(members: &[(&str, bool)]) -> Vec<u8> {
    let mut raw = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut raw);
        for (name, is_dir) in members {
            let mut h = tar::Header::new_gnu();
            if *is_dir {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_size(0);
                h.set_mode(0o755);
                h.set_cksum();
                ar.append_data(&mut h, name, std::io::empty()).unwrap();
            } else {
                h.set_size(2);
                h.set_mode(0o644);
                h.set_cksum();
                ar.append_data(&mut h, name, &b"hi"[..]).unwrap();
            }
        }
        ar.finish().unwrap();
    }
    let mut out = Vec::new();
    let mut w = lzma_rust2::XzWriter::new(&mut out, lzma_rust2::XzOptions::default()).unwrap();
    std::io::Write::write_all(&mut w, &raw).unwrap();
    w.finish().unwrap();
    out
}

#[test]
fn an_archive_with_nothing_to_install_is_not_reported_as_built() {
    let _on = TestMode::set(true);
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new("top");
    fake.archive = archive_of(&[
        ("top/", true),
        ("top/lib/", true),
        ("top/lib/libfoo@1.so.1", false),
        ("top/LICENSE", false),
    ]);
    fake.advertised = hex_sha(&fake.archive);
    let err = generate(&fake, "cuda-cudart", "13.4.92", "x86_64", dir.path()).unwrap_err();
    assert!(err.contains("cannot be packaged"), "{err}");
    // With a plain library beside the refused one, the package is built
    // and the refused member is not in it.
    fake.archive = archive_of(&[
        ("top/", true),
        ("top/lib/", true),
        ("top/lib/libfoo@1.so.1", false),
        ("top/lib/libok.so.1", false),
        ("top/LICENSE", false),
    ]);
    fake.advertised = hex_sha(&fake.archive);
    let fresh = tempfile::tempdir().unwrap();
    let g = generate(&fake, "cuda-cudart", "13.4.92", "x86_64", fresh.path()).unwrap();
    assert!(g.spec.contains("libok.so.1"), "{}", g.spec);
    assert!(!g.spec.contains("libfoo@"), "{}", g.spec);
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
fn the_archives_of_other_platforms_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz",
        "cuda_cudart-linux-x86_64-13.3.0-archive.tar.xz",
        "cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz",
        "cuda_cudart-linux-sbsa-13.3.0-archive.tar.xz",
    ] {
        std::fs::write(dir.path().join(f), b"x").unwrap();
    }
    // Keeping the sbsa release drops only the older sbsa release.
    let keep = dir
        .path()
        .join("cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz");
    remove_stale_archives(dir.path(), &keep);
    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "cuda_cudart-linux-sbsa-13.4.92-archive.tar.xz",
            "cuda_cudart-linux-x86_64-13.3.0-archive.tar.xz",
            "cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz",
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
