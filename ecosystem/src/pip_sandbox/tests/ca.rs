use super::*;
use std::fs;

#[test]
fn a_ca_bundle_outside_the_system_is_bound_at_its_own_path() {
    let mut f = fixture();
    let ca = f.home.join("corp-ca.pem");
    fs::write(&ca, "pem").unwrap();
    f.host
        .vars
        .push(("SSL_CERT_FILE".into(), ca.clone().into()));
    f.host
        .vars
        .push(("REQUESTS_CA_BUNDLE".into(), "/etc/ssl/ca-bundle.pem".into()));
    let b = binds(&build(&f));
    let ca = ca.to_string_lossy().into_owned();
    assert!(b.contains(&("--ro-bind".into(), ca.clone(), ca)));
    assert!(!b.iter().any(|(_, s, _)| s == "/etc/ssl/ca-bundle.pem"));
}

#[test]
fn a_ca_variable_pointing_at_credentials_is_refused() {
    let mut f = fixture();
    let key = f.home.join(".ssh/id_ed25519");
    f.host.vars.push(("SSL_CERT_FILE".into(), key.into()));
    let err = bwrap_args(&["pip"], &f.host).unwrap_err();
    assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
}

#[test]
fn ca_dir_entries_are_bound_one_by_one_and_only_usable_ones_are_passed_on() {
    let mut f = fixture();
    let good = f.home.join("corp-certs");
    fs::create_dir_all(&good).unwrap();
    let good_s = good.to_string_lossy().into_owned();
    let value = format!("/nonexistent/one:{good_s}::/nonexistent/two");
    f.host.vars.push(("SSL_CERT_DIR".into(), value.into()));
    let args = build(&f);
    let b = binds(&args);
    assert!(b.contains(&("--ro-bind".into(), good_s.clone(), good_s.clone())));
    assert!(!b.iter().any(|(_, s, _)| s.starts_with("/nonexistent")));
    // Only the entry that is bound survives in the sandbox's environment.
    let env = setenvs(&args);
    assert!(env.contains(&("SSL_CERT_DIR".into(), good_s)), "{env:?}");
}

#[test]
fn a_missing_ca_file_is_not_passed_on_to_the_sandbox() {
    let mut f = fixture();
    f.host
        .vars
        .push(("SSL_CERT_FILE".into(), "/nonexistent/ca.pem".into()));
    let args = build(&f);
    assert!(!setenvs(&args).iter().any(|(k, _)| k == "SSL_CERT_FILE"));
    assert!(
        !binds(&args)
            .iter()
            .any(|(_, s, _)| s.contains("nonexistent"))
    );
}

/// `/usr/..` is `/`, so the key is not under `/usr`: the check must see
/// the normalized path, not the prefix it was written with.
#[test]
fn a_ca_path_climbing_out_of_usr_to_credentials_is_refused() {
    let mut f = fixture();
    let key = f.home.join(".ssh/id_ed25519");
    let through = format!("/usr/..{}", key.display());
    f.host.vars.push(("SSL_CERT_FILE".into(), through.into()));
    let err = bwrap_args(&["pip"], &f.host).unwrap_err();
    assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
}

#[test]
fn a_ca_path_under_etc_ssl_is_still_accepted() {
    let path = "/etc/ssl/certs/ca-bundle.crt";
    if !Path::new(path).exists() {
        eprintln!("{path} is absent on this host; nothing to check");
        return;
    }
    let mut f = fixture();
    f.host.vars.push(("SSL_CERT_FILE".into(), path.into()));
    let args = build(&f);
    assert!(setenvs(&args).contains(&("SSL_CERT_FILE".into(), path.into())));
    assert!(!binds(&args).iter().any(|(_, s, _)| s.contains(".ssh")));
}

#[test]
fn normalize_lexically_resolves_dot_dot_and_refuses_climbing_above_root() {
    assert_eq!(
        normalize_lexically(Path::new("/usr/../home/x/./y")),
        Some(PathBuf::from("/home/x/y"))
    );
    assert_eq!(normalize_lexically(Path::new("/usr/../../x")), None);
    assert_eq!(
        normalize_lexically(Path::new("/")),
        Some(PathBuf::from("/"))
    );
    assert_eq!(normalize_lexically(Path::new("rel/x")), None);
}

#[test]
fn a_ca_dir_entry_naming_credentials_is_still_refused() {
    let mut f = fixture();
    let key = f.home.join(".ssh");
    let value = format!("{}:{}", f.home.join("corp-certs").display(), key.display());
    f.host.vars.push(("SSL_CERT_DIR".into(), value.into()));
    let err = bwrap_args(&["pip"], &f.host).unwrap_err();
    assert!(matches!(err, SandboxError::UnsafePath { .. }), "{err}");
}
