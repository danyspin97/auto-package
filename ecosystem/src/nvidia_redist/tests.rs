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
    assert_eq!(
        (m.product.as_str(), m.release_label.as_str()),
        ("cuda", "13.4.3")
    );
    assert_eq!(m.base_url, "https://x/redist");
    assert_eq!(m.components.len(), 4);
    let c = m
        .components
        .iter()
        .find(|c| c.key == "cuda_cudart")
        .unwrap();
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
    assert_eq!(
        cudnn
            .archive_for("linux-x86_64", Some("cuda11"))
            .unwrap()
            .cuda_variant
            .as_deref(),
        Some("11")
    );
    assert!(cudnn.archive_for("linux-x86_64", Some("13")).is_none());
    // No major requested: newest variant.
    assert_eq!(
        cudnn
            .archive_for("linux-x86_64", None)
            .unwrap()
            .cuda_variant
            .as_deref(),
        Some("12")
    );
}

#[test]
fn rejects_bad_manifest() {
    assert!(parse("[]", "b").is_err());
    assert!(parse(r#"{"release_label":"1"}"#, "b").is_err());
    let bad = r#"{"release_label":"1","release_product":"cuda","x":{"name":"n","version":"1",
        "linux-x86_64":{"relative_path":"p","sha256":"s","size":"big"}}}"#;
    // A bad archive entry is skipped, not fatal.
    let m = parse(bad, "b").unwrap();
    assert!(m.components[0].archives.is_empty());
}

#[test]
fn bad_windows_entry_keeps_linux() {
    let json = r#"{"release_label":"1","release_product":"cuda","x":{"name":"n","version":"1",
        "linux-x86_64":{"relative_path":"x/linux.tar.xz","sha256":"s","size":"5"},
        "windows-x86_64":{"relative_path":"x/win.tar.xz","sha256":"s","size":"big"}}}"#;
    let m = parse(json, "https://x/redist").unwrap();
    let c = &m.components[0];
    assert_eq!(c.archives.len(), 1);
    assert_eq!(c.archive_for("linux-x86_64", None).unwrap().size, 5);
    assert!(c.archive_for("windows-x86_64", None).is_none());
}

#[test]
fn bad_linux_entry_leaves_platform_missing() {
    // Missing sha256: the linux archive is dropped, the component survives.
    let json = r#"{"release_label":"1","release_product":"cuda","x":{"name":"n","version":"1",
        "linux-x86_64":{"relative_path":"x/linux.tar.xz","size":"5"},
        "linux-sbsa":{"relative_path":"x/sbsa.tar.xz","sha256":"s","size":"6"}}}"#;
    let m = parse(json, "https://x/redist").unwrap();
    let c = &m.components[0];
    assert!(c.archive_for("linux-x86_64", None).is_none());
    assert_eq!(c.archive_for("linux-sbsa", None).unwrap().size, 6);
}

#[test]
fn bad_cuda_variant_keeps_the_other() {
    let json = r#"{"release_label":"1","release_product":"cudnn","x":{"name":"n","version":"1",
        "linux-x86_64":{
            "cuda12":{"relative_path":"x/a.tar.xz","sha256":"s","size":"1"},
            "cuda13":{"relative_path":"/abs.tar.xz","sha256":"s","size":"2"}}}}"#;
    let m = parse(json, "https://x/redist").unwrap();
    let c = &m.components[0];
    assert!(c.archive_for("linux-x86_64", Some("13")).is_none());
    assert_eq!(c.archive_for("linux-x86_64", Some("12")).unwrap().size, 1);
}

/// A one-component manifest whose only archive has `relative_path` `path`.
fn manifest_with_path(path: &str) -> String {
    serde_json::json!({
        "release_label": "1",
        "release_product": "cuda",
        "x": {
            "name": "n",
            "version": "1",
            "linux-x86_64": {"relative_path": path, "sha256": "s", "size": "1"}
        }
    })
    .to_string()
}

#[test]
fn rejects_unsafe_relative_paths() {
    for bad in [
        "",
        "/abs/x.tar.xz",
        "C:/x.tar.xz",
        "https://evil.example/x.tar.xz",
        "a/b://c.tar.xz",
        "a//b.tar.xz",
        "a/b/",
        "a/./b.tar.xz",
        "a/../b.tar.xz",
        "a/..",
        "a\\b.tar.xz",
    ] {
        let m = parse(&manifest_with_path(bad), "https://x/redist").unwrap();
        assert!(
            m.components[0].archive_for("linux-x86_64", None).is_none(),
            "{bad:?} was accepted"
        );
    }
}

#[test]
fn accepts_plain_relative_paths() {
    for ok in [
        "cuda_cudart/linux-x86_64/a-archive.tar.xz",
        "a.tar.xz",
        "x/..y/a.tar.xz",
    ] {
        let m = parse(&manifest_with_path(ok), "https://x/redist/").unwrap();
        let a = &m.components[0].archives[0];
        assert_eq!(a.url(&m.base_url), format!("https://x/redist/{ok}"));
    }
}

#[test]
fn latest_is_version_aware() {
    let html = "<a href='redistrib_13.9.1.json'>x</a><a href='redistrib_13.10.0.json'>x</a>\
                <a href='redistrib_9.99.json'>x</a><a href='redistrib_13.4.3.json'>x</a>\
                <a href='cuda_cudart/'>c</a>";
    assert_eq!(
        latest_manifest_name(html).as_deref(),
        Some("redistrib_13.10.0.json")
    );
    assert_eq!(latest_manifest_name("<a href='cuda/'>"), None);
    // Four components, leading zeros, and names that are not versions.
    let html = "<a href='redistrib_12.4.1.json'>x</a><a href='redistrib_12.4.1.1.json'>x</a>\
                <a href='redistrib_12.04.0.json'>x</a><a href='redistrib_12.x.json'>x</a>\
                <a href='redistrib_13.beta.json'>x</a>";
    assert_eq!(
        latest_manifest_name(html).as_deref(),
        Some("redistrib_12.4.1.1.json")
    );
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
    for input in [
        "libcuda.so.1",
        "libcuda.so.1()(64bit)",
        "libc.so.6",
        "numpy",
        "nvidia-foo",
        "libcublas",
        "",
    ] {
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
            .unwrap_or_else(|| {
                panic!(
                    "{product:?} has no {key}: {:?}",
                    m.components.iter().map(|c| &c.key).collect::<Vec<_>>()
                )
            });
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
