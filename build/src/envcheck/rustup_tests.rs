//! The rustup rows against temporary rustup homes: a default that is set
//! and installed, no default, no toolchain, cargo that is not the proxy, and
//! a refused home. The helpers are the ones of `tests`.

use super::tests::{Run, ids, no_run, probe, row, triple};
use super::*;
use std::fs;
use std::path::Path;

fn rustup_probe<'a>(root: &Path, run: Run<'a>, services: &Path) -> Probe<'a> {
    let mut p = probe(OsString::new(), services, run);
    let home = root.join("home");
    p.rustup_vars = HostVars {
        rustup_home: None,
        home: Some(home),
        cargo_home: None,
    };
    p.cargo_is_rustup = true;
    p
}

/// A rustup home under `root/home/.rustup` with the given toolchains and an
/// optional `settings.toml`.
fn rustup_home(root: &Path, toolchains: &[String], settings: Option<&str>) -> PathBuf {
    let rustup = root.join("home").join(".rustup");
    fs::create_dir_all(rustup.join("toolchains")).unwrap();
    for name in toolchains {
        fs::create_dir_all(rustup.join("toolchains").join(name)).unwrap();
    }
    if let Some(text) = settings {
        fs::write(rustup.join("settings.toml"), text).unwrap();
    }
    rustup
}

fn rust_needs() -> Needs {
    Needs {
        rust: true,
        ..Needs::default()
    }
}

#[test]
fn a_default_that_is_installed_is_reported_and_the_sandbox_uses_it() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    let name = format!("stable-{}", triple());
    rustup_home(
        root.path(),
        std::slice::from_ref(&name),
        Some(&format!("default_toolchain = \"{name}\"\n")),
    );
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    assert_eq!(
        row(&rows, "rustup-default").outcome,
        Outcome::Ok(format!("default toolchain {name}"))
    );
    assert_eq!(
        row(&rows, "sandbox-toolchain").outcome,
        Outcome::Ok(format!("{name} (the host default)"))
    );
}

#[test]
fn no_default_with_stable_installed_says_what_the_sandbox_will_use() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    let name = format!("stable-{}", triple());
    rustup_home(root.path(), std::slice::from_ref(&name), None);
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    match &row(&rows, "rustup-default").outcome {
        Outcome::WarnText(text) => {
            assert!(text.contains("rustup has no default toolchain"), "{text}");
            assert!(
                text.contains(&format!("the sandbox will use {name} (stable)")),
                "{text}"
            );
            assert!(!text.contains("rustup default stable"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        row(&rows, "sandbox-toolchain").outcome,
        Outcome::Ok(format!("{name} (stable)"))
    );
}

#[test]
fn no_default_with_only_nightly_never_suggests_a_missing_stable() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    let nightly = format!("nightly-{}", triple());
    rustup_home(root.path(), std::slice::from_ref(&nightly), None);
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    match &row(&rows, "rustup-default").outcome {
        Outcome::WarnText(text) => {
            assert!(text.contains("no stable toolchain is installed"), "{text}");
            assert!(text.contains("rustup toolchain install stable"), "{text}");
            assert!(!text.contains("rustup default stable"), "{text}");
            assert!(!text.contains("will use"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        row(&rows, "sandbox-toolchain").outcome,
        Outcome::Missing(Cause::ToolchainUnset {
            tool: "cargo".to_string()
        })
    );
}

#[test]
fn a_default_that_is_not_installed_says_what_the_sandbox_will_use() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    let stable = format!("stable-{}", triple());
    let uninstalled = format!("nightly-{}", triple());
    rustup_home(
        root.path(),
        std::slice::from_ref(&stable),
        Some(&format!("default_toolchain = \"{uninstalled}\"\n")),
    );
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    match &row(&rows, "rustup-default").outcome {
        Outcome::WarnText(text) => {
            assert!(
                text.contains(&format!("default toolchain {uninstalled} is not installed")),
                "{text}"
            );
            assert!(
                text.contains(&format!("the sandbox will use {stable} (stable)")),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_toolchain_at_all_is_missing_for_both_rows() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    rustup_home(root.path(), &[], None);
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    let unset = Outcome::Missing(Cause::ToolchainUnset {
        tool: "cargo".to_string(),
    });
    assert_eq!(row(&rows, "rustup-default").outcome, unset);
    assert_eq!(row(&rows, "sandbox-toolchain").outcome, unset);
    assert_eq!(
        row(&rows, "sandbox-toolchain").hint.as_deref(),
        Some("rustup toolchain install stable")
    );

    let root = tempfile::tempdir().unwrap();
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    assert_eq!(row(&rows, "rustup-default").outcome, unset);
}

#[test]
fn cargo_that_is_not_the_rustup_proxy_hides_the_rustup_row() {
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    rustup_home(root.path(), &[], None);
    let mut p = rustup_probe(root.path(), &no_run, services.path());
    p.cargo_is_rustup = false;
    let rows = run(&rust_needs(), &p);
    assert!(
        rows.iter().all(|r| r.id != "rustup-default"),
        "{:?}",
        ids(&rows)
    );
    assert_eq!(
        row(&rows, "sandbox-toolchain").outcome,
        Outcome::Ok("cargo is a regular binary, no rustup involved".to_string())
    );
}

#[test]
fn a_refused_rustup_home_is_missing_for_the_sandbox_and_hides_the_default() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let services = tempfile::tempdir().unwrap();
    let name = format!("stable-{}", triple());
    let rustup = rustup_home(root.path(), std::slice::from_ref(&name), None);
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o777)).unwrap();
    let p = rustup_probe(root.path(), &no_run, services.path());
    let rows = run(&rust_needs(), &p);
    assert!(rows.iter().all(|r| r.id != "rustup-default"));
    match &row(&rows, "sandbox-toolchain").outcome {
        Outcome::Missing(Cause::SandboxUnavailable { why }) => {
            assert!(why.contains("was not used"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}
