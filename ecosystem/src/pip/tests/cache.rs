use super::*;

#[test]
fn cache_key_folds_interpreter_and_platform_into_a_short_hash() {
    let key = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert!(key.starts_with("httpx-0.28.1-"), "{key}");
    assert!(key.ends_with(".json"), "{key}");
    // 12-char hex hash segment between the version and `.json`.
    let stem = key.strip_suffix(".json").unwrap();
    let hash = stem.rsplit('-').next().unwrap();
    assert_eq!(hash.len(), 12, "{key}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
}

#[test]
fn cache_key_changes_when_interpreter_changes() {
    let a = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let b = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.14.0",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_ne!(a, b, "interpreter must change the cache key");
}

#[test]
fn cache_key_changes_when_platform_changes() {
    let a = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let b = cache_key(
        "httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-aarch64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_ne!(a, b, "platform must change the cache key");
}

#[test]
fn cache_key_is_deterministic_for_the_same_inputs() {
    let a = cache_key(
        "Httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let b = cache_key(
        "Httpx",
        Some("0.28.1"),
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_eq!(a, b);
}

#[test]
fn cache_key_lowercases_the_package_name() {
    // Avoids `MarkupSafe` vs `markupsafe` producing two different cache
    // entries for the same release.
    let a = cache_key(
        "MarkupSafe",
        Some("3.0.3"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let b = cache_key(
        "markupsafe",
        Some("3.0.3"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_eq!(a, b);
}

#[test]
fn cache_key_distinguishes_latest_from_pinned() {
    let latest = cache_key(
        "httpx",
        None,
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let pinned = cache_key(
        "httpx",
        Some("0.28.1"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_ne!(latest, pinned);
    assert!(latest.contains("httpx-latest-"), "{latest}");
}

#[test]
fn cache_dir_is_always_absolute() {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    let tmp = PathBuf::from("/tmp");
    let o = |s: &str| Some(OsString::from(s));
    let d = |x, h| cache_dir_from(x, h, tmp.clone(), 1000);
    assert_eq!(
        d(o("/x"), o("/h")),
        Path::new("/x/auto-package/pip-resolve")
    );
    assert_eq!(
        d(o(""), o("/h")),
        Path::new("/h/.cache/auto-package/pip-resolve")
    );
    assert_eq!(
        d(o("rel"), None),
        Path::new("/tmp/auto-package-cache-1000/auto-package/pip-resolve")
    );
    assert!(d(None, o("relhome")).is_absolute());
    assert!(d(None, None).is_absolute());
}

#[test]
fn cache_key_cannot_escape_the_cache_dir() {
    let key = cache_key(
        "../../etc/passwd",
        Some("1/../2"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert!(!key.contains('/'), "{key}");
    assert!(!key.contains(".."), "{key}");
    let a = cache_key(
        "Foo_Bar.baz",
        Some("1.0"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    let b = cache_key(
        "foo-bar-baz",
        Some("1.0"),
        "p",
        "l",
        ResolveMode::Sandboxed,
        &BTreeMap::new(),
        None,
    );
    assert_eq!(a, b);
    assert!(a.starts_with("foo-bar-baz-1.0-"), "{a}");
}

fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect()
}

#[test]
fn cache_key_changes_when_the_index_url_changes() {
    // Resolving against mirror A and then mirror B must not share an entry.
    let key = |idx: &str| {
        cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &env_of(&[("PIP_INDEX_URL", idx)]),
            None,
        )
    };
    let a = key("https://mirror-a.invalid/simple");
    let b = key("https://mirror-b.invalid/simple");
    assert_ne!(a, b, "PIP_INDEX_URL must change the cache key");
    assert_eq!(a, key("https://mirror-a.invalid/simple"));
}

#[test]
fn every_resolution_variable_changes_the_cache_key() {
    let key = |vars: &BTreeMap<OsString, OsString>| {
        cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            vars,
            None,
        )
    };
    let unset = key(&BTreeMap::new());
    for var in RESOLUTION_ENV {
        let set = key(&env_of(&[(*var, "https://example.invalid/x")]));
        assert_ne!(unset, set, "{var} must change the cache key");
    }
}

#[test]
fn unset_resolution_variables_give_a_stable_key() {
    let key = || {
        cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        )
    };
    assert_eq!(key(), key());
}

#[test]
fn index_credentials_are_hashed_not_printed() {
    let key = cache_key(
        "httpx",
        None,
        "Python 3.13.1",
        "linux-x86_64",
        ResolveMode::Sandboxed,
        &env_of(&[("PIP_INDEX_URL", "https://user:hunter2@idx.invalid/simple")]),
        None,
    );
    assert!(!key.contains("hunter2"), "{key}");
    assert!(!key.contains("idx.invalid"), "{key}");
}

#[test]
fn the_content_of_the_pip_config_file_changes_the_cache_key() {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("private-pip.conf");
    let conf_path = conf.to_str().unwrap();
    let key = |body: &str| {
        std::fs::write(&conf, body).unwrap();
        cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &env_of(&[("PIP_CONFIG_FILE", conf_path)]),
            None,
        )
    };
    let a = key("[global]\nindex-url = https://a.invalid/simple\n");
    let b = key("[global]\nindex-url = https://b.invalid/simple\n");
    assert_ne!(a, b, "config content must change the cache key");
    assert_eq!(a, key("[global]\nindex-url = https://a.invalid/simple\n"));
}

#[test]
fn the_user_pip_conf_under_home_changes_the_cache_key() {
    let home = tempfile::tempdir().unwrap();
    let home_str = home.path().to_str().unwrap();
    let conf_dir = home.path().join(".config/pip");
    std::fs::create_dir_all(&conf_dir).unwrap();
    let key = |body: &str| {
        std::fs::write(conf_dir.join("pip.conf"), body).unwrap();
        cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &env_of(&[("HOME", home_str)]),
            None,
        )
    };
    let a = key("[global]\nextra-index-url = https://a.invalid/simple\n");
    let b = key("[global]\nextra-index-url = https://b.invalid/simple\n");
    assert_ne!(a, b, "~/.config/pip/pip.conf must change the cache key");
}

#[test]
fn system_config_content_changes_the_cache_key() {
    // `/etc/pip.conf` itself cannot be written by a test, so the same
    // digest path is exercised with an injected candidate list.
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("etc-pip.conf");
    let key = |body: &str| {
        std::fs::write(&conf, body).unwrap();
        cache_key_with_configs(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            &[Some(conf.clone())],
        )
    };
    let a = key("[global]\nindex-url = https://a.invalid/simple\n");
    let b = key("[global]\nindex-url = https://b.invalid/simple\n");
    assert_ne!(a, b, "a system config's content must change the cache key");
    assert_eq!(a, key("[global]\nindex-url = https://a.invalid/simple\n"));
}

#[test]
fn missing_config_files_give_a_stable_key() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such.conf");
    let key = |configs: &[Option<std::path::PathBuf>]| {
        cache_key_with_configs(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            configs,
        )
    };
    let absent = key(&[None, Some(missing.clone()), None]);
    assert_eq!(absent, key(&[None, Some(missing.clone()), None]));
    // A directory is not a config file either.
    assert_eq!(absent, key(&[None, Some(dir.path().to_path_buf()), None]));
    // Once the file exists, the key changes.
    std::fs::write(&missing, "[global]\n").unwrap();
    assert_ne!(absent, key(&[None, Some(missing.clone()), None]));
}

#[test]
fn config_candidates_are_in_a_fixed_order() {
    let vars: BTreeMap<OsString, OsString> = [
        ("HOME", "/h"),
        ("XDG_CONFIG_HOME", "/x"),
        ("PIP_CONFIG_FILE", "/p.conf"),
        ("XDG_CONFIG_DIRS", "/a:rel:/b"),
    ]
    .iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
    .collect();
    let got = config_candidates(&vars, Some(std::path::Path::new("/venv")));
    let p = |s: &str| Some(std::path::PathBuf::from(s));
    assert_eq!(
        got,
        vec![
            p("/p.conf"),
            p("/x/pip/pip.conf"),
            p("/h/.config/pip/pip.conf"),
            p("/h/.pip/pip.conf"),
            p("/etc/pip.conf"),
            p("/etc/xdg/pip/pip.conf"),
            p("/usr/local/etc/xdg/pip/pip.conf"),
            p("/usr/etc/xdg/pip/pip.conf"),
            p("/a/pip/pip.conf"),
            None, // relative XDG_CONFIG_DIRS entries are ignored
            p("/b/pip/pip.conf"),
            p("/venv/pip.conf"),
        ]
    );
}

#[test]
fn resolve_modes_do_not_share_a_cache_entry() {
    // A wheels-only closure lacks sdist-only releases, and a closure
    // resolved with sdists may hold releases a wheels-only run must
    // reject: each mode gets its own entry.
    let key = |mode| {
        cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            mode,
            &BTreeMap::new(),
            None,
        )
    };
    let sandboxed = key(ResolveMode::Sandboxed);
    let wheels = key(ResolveMode::WheelsOnly);
    let unsandboxed = key(ResolveMode::Unsandboxed);
    assert_ne!(sandboxed, wheels);
    assert_ne!(sandboxed, unsandboxed);
    assert_ne!(wheels, unsandboxed);
}

#[test]
fn an_entry_recorded_for_312_is_not_served_to_a_313_host() {
    // A closure resolved by `python3.12 -m pip` (the host's default was
    // incompatible) is stored under 3.12's key. A later run on a 3.13 host
    // must miss and resolve again, not get a closure from another Python.
    let dir = tempfile::tempdir().unwrap();
    let nodes = vec![Node {
        name: "httpx".into(),
        version: "0.28.1".into(),
        sha256: "ab".into(),
        requires_dist: vec![],
        requires_python: None,
        is_requested: true,
        is_yanked: false,
    }];
    let mode = ResolveMode::Sandboxed;
    save_cached_resolve(
        dir.path(),
        "httpx",
        Some("0.28.1"),
        "Python 3.12.7",
        None,
        mode,
        &nodes,
    );
    assert_eq!(
        load_cached_resolve(
            dir.path(),
            "httpx",
            Some("0.28.1"),
            "Python 3.12.7",
            None,
            mode
        ),
        Some(nodes)
    );
    assert_eq!(
        load_cached_resolve(
            dir.path(),
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            None,
            mode
        ),
        None
    );
}
