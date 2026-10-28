use super::*;

mod cache;
mod probe;

/// Trimmed from a real `pip install --dry-run --report -` on httpx 0.28.1.
/// The wheel URL and long description are dropped; every other field is
/// verbatim, including the empty `requires_dist` on leaf packages.
const HTTPX_REPORT: &str = r#"{
  "version": "1",
  "pip_version": "26.2",
  "install": [
    {
      "download_info": {
        "url": "https://files.pythonhosted.org/packages/httpx-0.28.1-py3-none-any.whl",
        "archive_info": {
          "hashes": {"sha256": "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"},
          "hash": "sha256=d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
        }
      },
      "is_direct": false, "is_yanked": false, "requested": true,
      "metadata": {
        "metadata_version": "2.3", "name": "httpx", "version": "0.28.1",
        "requires_python": ">=3.8",
        "requires_dist": [
          "anyio", "certifi", "httpcore==1.*", "idna",
          "brotli; (platform_python_implementation == 'CPython') and extra == 'brotli'",
          "click==8.*; extra == 'cli'", "h2<5,>=3; extra == 'http2'"
        ]
      }
    },
    {
      "download_info": {
        "url": "https://files.pythonhosted.org/packages/h11-0.16.0-py3-none-any.whl",
        "archive_info": {
          "hashes": {"sha256": "63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"},
          "hash": "sha256=63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"
        }
      },
      "is_direct": false, "is_yanked": false, "requested": false,
      "metadata": {
        "metadata_version": "2.1", "name": "h11", "version": "0.16.0",
        "requires_python": ">=3.8",
        "requires_dist": []
      }
    },
    {
      "download_info": {
        "url": "https://files.pythonhosted.org/packages/certifi-2026.7.22-py3-none-any.whl",
        "archive_info": {
          "hashes": {"sha256": "abc123"},
          "hash": "sha256=abc123"
        }
      },
      "is_direct": false, "is_yanked": false, "requested": false,
      "metadata": {
        "metadata_version": "2.1", "name": "certifi", "version": "2026.7.22"
      }
    }
  ],
  "environment": {
    "python_version": "3.13"
  }
}"#;

fn parse(json: &str) -> Vec<Node> {
    let report: Report = serde_json::from_str(json).unwrap();
    report
        .install
        .into_iter()
        .map(|e| {
            let sha256 = e
                .download_info
                .archive_info
                .hashes
                .sha256
                .expect("fixture has sha256");
            Node {
                name: e.metadata.name,
                version: e.metadata.version,
                sha256,
                requires_dist: e.metadata.requires_dist,
                requires_python: e.metadata.requires_python,
                is_requested: e.requested,
                is_yanked: e.is_yanked,
            }
        })
        .collect()
}

#[test]
fn parses_root_and_transitive_nodes() {
    let nodes = parse(HTTPX_REPORT);
    assert_eq!(nodes.len(), 3);
}

#[test]
fn root_is_marked_requested() {
    let nodes = parse(HTTPX_REPORT);
    let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
    assert!(root.is_requested);
    assert!(!nodes.iter().find(|n| n.name == "h11").unwrap().is_requested);
}

#[test]
fn markers_and_extras_are_kept_unevaluated() {
    let nodes = parse(HTTPX_REPORT);
    let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
    assert!(
        root.requires_dist
            .iter()
            .any(|r| r.contains("extra == 'cli'"))
    );
    assert!(
        root.requires_dist
            .iter()
            .any(|r| r.contains("platform_python_implementation"))
    );
}

#[test]
fn leaf_with_no_requires_dist_field_gives_empty_vec() {
    // `certifi` in the fixture has no `requires_dist` key at all.
    let nodes = parse(HTTPX_REPORT);
    let leaf = nodes.iter().find(|n| n.name == "certifi").unwrap();
    assert!(leaf.requires_dist.is_empty());
}

#[test]
fn sha256_is_extracted_from_hashes_not_the_legacy_hash_field() {
    let nodes = parse(HTTPX_REPORT);
    assert_eq!(
        nodes.iter().find(|n| n.name == "httpx").unwrap().sha256,
        "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
    );
}

#[test]
fn resolution_accepts_sdists_without_any_opt_in() {
    let args = pip_report_args("httpx==0.28.1", false);
    assert!(!args.iter().any(|a| a.contains("only-binary")), "{args:?}");
    assert_eq!(args[args.len() - 2], "--");
    assert_eq!(args.last(), Some(&"httpx==0.28.1"));
}

#[test]
fn pip_child_sees_allowed_variables_and_no_credentials() {
    let vars = [
        ("PATH", std::env::var("PATH").unwrap_or_default()),
        ("HOME", "/home/u".to_string()),
        ("HTTPS_PROXY", "http://proxy.invalid:3128".to_string()),
        ("PIP_INDEX_URL", "https://idx.invalid/simple".to_string()),
        ("SSL_CERT_FILE", "/etc/ssl/ca.pem".to_string()),
        ("LANG", "C.UTF-8".to_string()),
        ("GITHUB_TOKEN", "fake-github-secret".to_string()),
        ("OSC_PASSWORD", "fake-osc-secret".to_string()),
        ("ANTHROPIC_API_KEY", "fake-anthropic-secret".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let mut cmd = pip_command(&["env"], vars);
    let out = cmd.stdin(Stdio::null()).output().expect("env must run");
    assert!(out.status.success());
    let seen = String::from_utf8_lossy(&out.stdout);
    let keys: Vec<&str> = seen
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();

    for want in [
        "PATH",
        "HOME",
        "HTTPS_PROXY",
        "PIP_INDEX_URL",
        "SSL_CERT_FILE",
        "LANG",
    ] {
        assert!(keys.contains(&want), "{want} missing from {keys:?}");
    }
    assert!(!seen.contains("fake-github-secret"), "{seen}");
    assert!(!seen.contains("fake-osc-secret"), "{seen}");
    assert!(!seen.contains("fake-anthropic-secret"), "{seen}");
    for k in &keys {
        assert!(PIP_ENV_ALLOWLIST.contains(k), "{k} is not allowlisted");
    }
}

#[test]
fn pip_inherits_only_allowlisted_environment() {
    let vars = [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/u"),
        ("OSC_PASSWORD", "x"),
        ("GITHUB_TOKEN", "x"),
        ("ANTHROPIC_API_KEY", "x"),
        ("PIP_INDEX_URL", "https://idx"),
    ]
    .into_iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let kept: Vec<String> = pip_env(vars)
        .into_iter()
        .map(|(k, _)| k.into_string().unwrap())
        .collect();
    assert_eq!(kept, ["PATH", "HOME", "PIP_INDEX_URL"]);
}

#[test]
fn unknown_top_level_fields_do_not_break_parsing() {
    let json = HTTPX_REPORT.replace(
        r#""version": "1""#,
        r#""version": "1", "future_field": {"a": 1}"#,
    );
    assert_eq!(parse(&json).len(), 3);
}
