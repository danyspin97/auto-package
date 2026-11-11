use super::*;
use std::collections::BTreeMap;

const STABLE: &str = "stable-x86_64-unknown-linux-gnu";
const OLD: &str = "1.80.0-x86_64-unknown-linux-gnu";

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn pick(pin: Option<&str>, installed: &[&str], default: Option<&str>) -> Choice {
    select_toolchain(&Inputs {
        pin_file: pin,
        installed: &names(installed),
        host_default: default,
        cargo_is_rustup: true,
    })
}

fn selected(c: Choice) -> (String, String, Vec<String>) {
    match c {
        Choice::Selected {
            name,
            why,
            warnings,
        } => (name, why, warnings),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_installed_pin_wins_over_the_host_default() {
    let (name, why, warnings) = selected(pick(Some("1.80.0\n"), &[STABLE, OLD], Some(STABLE)));
    assert_eq!(name, OLD);
    assert!(why.contains("pinned") && why.contains("1.80.0"), "{why}");
    assert!(warnings.is_empty());
}

#[test]
fn a_pin_that_is_not_installed_falls_back_with_a_warning() {
    let (name, why, warnings) = selected(pick(Some("1.70.0"), &[STABLE], Some(STABLE)));
    assert_eq!(name, STABLE);
    assert!(why.starts_with("fallback"), "{why}");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("1.70.0") && warnings[0].contains("not installed"));
    // With no host default it falls back to stable.
    let (name, why, _) = selected(pick(Some("1.70.0"), &[OLD, STABLE], None));
    assert_eq!(name, STABLE);
    assert!(why.contains("stable"), "{why}");
}

#[test]
fn the_host_default_is_used_without_a_pin() {
    let (name, why, warnings) = selected(pick(None, &[STABLE, OLD], Some(OLD)));
    assert_eq!(name, OLD);
    assert!(
        why.contains("host default") && !why.contains("fallback"),
        "{why}"
    );
    assert!(warnings.is_empty());
    // A default that is not installed is skipped.
    let (name, _, _) = selected(pick(None, &[STABLE], Some("nightly")));
    assert_eq!(name, STABLE);
}

#[test]
fn stable_alone_is_selected_by_its_channel_prefix() {
    let (name, why, _) = selected(pick(None, &[STABLE], None));
    assert_eq!(name, STABLE);
    assert!(why.contains("stable"));
}

#[test]
fn nothing_usable_installed_is_no_toolchain() {
    assert_eq!(pick(None, &[], Some(STABLE)), Choice::NoToolchain);
    // One installed toolchain that is neither pinned, default nor stable.
    assert_eq!(pick(None, &[OLD], None), Choice::NoToolchain);
    assert_eq!(pick(Some("1.70.0"), &[], None), Choice::NoToolchain);
}

#[test]
fn a_real_cargo_selects_nothing() {
    let choice = select_toolchain(&Inputs {
        pin_file: Some("1.80.0"),
        installed: &names(&[STABLE]),
        host_default: Some(STABLE),
        cargo_is_rustup: false,
    });
    assert_eq!(choice, Choice::PlainCargo);
    assert!(choice_env(&choice).is_empty());
    assert!(choice_env(&Choice::NoToolchain).is_empty());
}

#[test]
fn both_pin_formats_are_read() {
    assert_eq!(pinned_channel("stable\n"), Ok(Some("stable".into())));
    assert_eq!(
        pinned_channel("# note\n"),
        Err("`# note` is not a toolchain name".into())
    );
    assert_eq!(
        pinned_channel("[toolchain]\nchannel = \"1.80.0\"\ncomponents = [\"clippy\"]\n"),
        Ok(Some("1.80.0".into()))
    );
    assert_eq!(pinned_channel("[toolchain]\ncomponents = []\n"), Ok(None));
    assert_eq!(pinned_channel("  \n"), Ok(None));
}

#[test]
fn a_malformed_pin_is_ignored_with_a_warning() {
    for bad in [
        "[toolchain\nchannel =",
        "stable\nnightly\n",
        "../../etc",
        "[toolchain]\nchannel = 3\n",
        "channel = \"x\"\n",
    ] {
        assert!(pinned_channel(bad).is_err(), "{bad:?}");
        let (name, _, warnings) = selected(pick(Some(bad), &[STABLE], Some(STABLE)));
        assert_eq!(name, STABLE);
        assert!(warnings[0].starts_with("ignoring"), "{warnings:?}");
    }
}

#[test]
fn a_channel_does_not_match_a_dated_toolchain() {
    let installed = names(&[
        "nightly-2024-01-01-x86_64-unknown-linux-gnu",
        "stable-x86_64-unknown-linux-gnu",
    ]);
    assert_eq!(installed_match("nightly", &installed), None);
    assert_eq!(
        installed_match("nightly-2024-01-01", &installed),
        Some("nightly-2024-01-01-x86_64-unknown-linux-gnu")
    );
    assert_eq!(installed_match("stable", &installed), Some(STABLE));
    assert_eq!(installed_match(STABLE, &installed), Some(STABLE));
}

#[test]
fn a_selection_sets_the_toolchain_and_forbids_downloads() {
    let c = pick(None, &[STABLE], None);
    assert_eq!(
        choice_env(&c),
        [
            ("RUSTUP_TOOLCHAIN".to_string(), STABLE.to_string()),
            ("RUSTUP_AUTO_INSTALL".to_string(), "0".to_string()),
        ]
    );
}

#[test]
fn the_host_files_are_read_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    fs::create_dir_all(r.join("toolchains").join(STABLE)).unwrap();
    fs::create_dir_all(r.join("toolchains").join(OLD)).unwrap();
    fs::write(
        r.join("settings.toml"),
        format!("version = \"12\"\ndefault_toolchain = \"{OLD}\"\n"),
    )
    .unwrap();
    assert_eq!(installed_toolchains(r), (names(&[OLD, STABLE]), Vec::new()));
    assert_eq!(host_default(r).as_deref(), Some(OLD));
    assert!(installed_toolchains(&r.join("none")).0.is_empty());
    assert_eq!(host_default(&r.join("none")), None);
}

#[test]
fn the_pin_is_read_from_the_src_directory_inside_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = fs::canonicalize(dir.path()).unwrap();
    fs::create_dir_all(checkout.join("pkg-1.0")).unwrap();
    let p = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
    assert_eq!(
        source_dir(&p("srcdir", "pkg-1.0"), &checkout),
        Some(checkout.join("pkg-1.0"))
    );
    assert_eq!(source_dir(&p("src", "missing"), &checkout), None);
    assert_eq!(source_dir(&p("srctar", "pkg.tar.gz"), &checkout), None);
    std::os::unix::fs::symlink("/", checkout.join("out")).unwrap();
    assert_eq!(source_dir(&p("src", "out"), &checkout), None);
}

fn call(name: &str) -> ServiceCall {
    ServiceCall {
        name: name.to_string(),
        params: Vec::new(),
    }
}

#[test]
fn services_that_do_not_run_cargo_get_no_rustup_setup() {
    let dir = tempfile::tempdir().unwrap();
    let services: Vec<_> = [
        "tar_scm",
        "obs_scm",
        "recompress",
        "set_version",
        "download_files",
    ]
    .map(call)
    .into();
    let setups = cargo_setups(&services, dir.path(), Emit::quiet()).unwrap();
    assert_eq!(setups.len(), services.len());
    assert!(setups.iter().all(|s| *s == CargoSetup::default()));
}

#[test]
fn a_sandbox_of_a_non_cargo_service_has_no_rustup_env_or_bind() {
    let setups =
        cargo_setups(&[call("tar_scm")], Path::new("/nonexistent"), Emit::quiet()).unwrap();
    let setup = &setups[0];
    let p = Profile {
        checkout: Path::new("/work/proj/pkg"),
        scratch: Path::new("/work/proj/pkg/.ap-service-x"),
        proxy_dir: Path::new("/tmp/ap-service-proxy-x"),
        extra_env: &setup.env,
        ro_binds: &setup.ro_binds,
        hosts_file: Path::new("/tmp/ap-service-proxy-x/hosts"),
        tunnels: &[],
    };
    let args = bwrap_args(&p, &["tar_scm".to_string()]).unwrap();
    assert!(
        !args
            .iter()
            .any(|a| a.contains("RUSTUP") || a.contains("rustup"))
    );
}

fn rustup_with(toolchains: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for t in toolchains {
        fs::create_dir_all(dir.path().join("toolchains").join(t)).unwrap();
    }
    dir
}

#[test]
fn a_usable_home_is_bound_and_names_its_toolchain() {
    let dir = rustup_with(&[STABLE]);
    let home = fs::canonicalize(dir.path()).unwrap();
    let setup = cargo_setup(
        &call("cargo_vendor"),
        Path::new("/co"),
        RustupHome::Usable(home.clone()),
    )
    .unwrap();
    assert_eq!(setup.ro_binds, std::slice::from_ref(&home));
    let env: BTreeMap<_, _> = setup.env.iter().cloned().collect();
    assert_eq!(env["RUSTUP_HOME"], home.to_str().unwrap());
    assert_eq!(env["RUSTUP_TOOLCHAIN"], STABLE);
    assert_eq!(env["RUSTUP_AUTO_INSTALL"], "0");
    assert!(
        setup
            .info
            .unwrap()
            .starts_with("vendoring uses toolchain stable")
    );
}

#[test]
fn a_refused_home_fails_with_its_reason_and_not_as_an_unset_toolchain() {
    let refused = Refused {
        path: PathBuf::from("/h/.rustup"),
        why: "it is owned by another user".into(),
    };
    let (warnings, err) = cargo_setup(
        &call("cargo_vendor"),
        Path::new("/co"),
        RustupHome::Refused(refused),
    )
    .unwrap_err();
    assert_eq!(
        warnings,
        ["rustup home /h/.rustup was not used: it is owned by another user"]
    );
    assert!(matches!(err, Error::RustupHomeRefused { .. }));
    assert_eq!(
        err.to_string(),
        "rustup home /h/.rustup was not used: it is owned by another user"
    );
    assert!(matches!(
        err.remedy().map(|r| r.cause),
        Some(ap_core::remedy::Cause::SandboxUnavailable { .. })
    ));
}

#[test]
fn an_absent_home_or_one_without_toolchains_is_an_unset_toolchain() {
    let empty = rustup_with(&[]);
    for home in [
        RustupHome::Absent,
        RustupHome::Usable(empty.path().to_path_buf()),
    ] {
        let (_, err) = cargo_setup(&call("cargo_vendor"), Path::new("/co"), home).unwrap_err();
        assert!(matches!(err, Error::ToolchainUnset { ref tool } if tool == "cargo"));
    }
}

#[test]
fn a_toolchain_linked_outside_the_home_is_skipped_with_a_warning() {
    let outside = tempfile::tempdir().unwrap();
    let dir = rustup_with(&[OLD]);
    std::os::unix::fs::symlink(outside.path(), dir.path().join("toolchains").join(STABLE)).unwrap();
    std::os::unix::fs::symlink("/nonexistent", dir.path().join("toolchains").join("broken"))
        .unwrap();
    let (installed, warnings) = installed_toolchains(dir.path());
    assert_eq!(installed, names(&[OLD]));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().any(|w| w
        == &format!(
            "toolchain {STABLE} is linked outside the rustup home and is not visible to the sandbox"
        )));
    // The host default is a linked toolchain: it is not chosen.
    fs::write(
        dir.path().join("settings.toml"),
        format!("default_toolchain = \"{STABLE}\"\n"),
    )
    .unwrap();
    let setup = cargo_setup(
        &call("cargo_vendor"),
        Path::new("/co"),
        RustupHome::Usable(fs::canonicalize(dir.path()).unwrap()),
    );
    // Only OLD is visible, and it is neither pinned, default nor stable.
    assert!(matches!(setup, Err((_, Error::ToolchainUnset { .. }))));
}

#[test]
fn a_link_inside_the_home_is_kept() {
    let dir = rustup_with(&[OLD]);
    std::os::unix::fs::symlink(OLD, dir.path().join("toolchains").join("alias")).unwrap();
    assert_eq!(installed_toolchains(dir.path()).0, names(&[OLD, "alias"]));
}

#[test]
fn a_suffix_must_be_a_target_triple() {
    let installed = names(&[
        "stable-foo",
        "stable-x86_64-unknown-linux-gnu",
        "stable-a-b",
    ]);
    assert_eq!(installed_match("stable", &installed), Some(STABLE));
    assert_eq!(installed_match("stable", &names(&["stable-foo"])), None);
    assert_eq!(installed_match("stable", &names(&["stable-1-2-3"])), None);
    assert_eq!(
        installed_match("stable", &names(&["stable-aarch64-apple-darwin"])),
        Some("stable-aarch64-apple-darwin")
    );
    assert_eq!(installed_match("", &names(&[""])), None);
    assert_eq!(installed_match("", &installed), None);
}

#[test]
fn the_host_triple_is_preferred_over_the_alphabetical_first() {
    let host = format!("stable-{}-unknown-linux-gnu", std::env::consts::ARCH);
    let installed = names(&["stable-a0-unknown-linux-gnu", &host]);
    assert_eq!(installed_match("stable", &installed), Some(host.as_str()));
}

#[test]
fn an_empty_default_toolchain_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("settings.toml"),
        "default_toolchain = \"\"\n",
    )
    .unwrap();
    assert_eq!(host_default(dir.path()), None);
    assert_eq!(pick(None, &[OLD], Some("")), Choice::NoToolchain);
}

#[test]
fn a_cargo_that_is_not_a_rustup_link_is_plain() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("cargo");
    fs::write(&real, "").unwrap();
    let proxy = dir.path().join("rustup");
    fs::write(&proxy, "").unwrap();
    let link = dir.path().join("cargo-link");
    std::os::unix::fs::symlink("rustup", &link).unwrap();
    assert!(!cargo_is_rustup(&[real.to_str().unwrap()]));
    assert!(cargo_is_rustup(&[link.to_str().unwrap()]));
    assert!(!cargo_is_rustup(&["/nonexistent/cargo"]));
}

#[test]
fn no_toolchain_ends_as_an_environment_failure_with_a_remedy() {
    let err = no_toolchain_error(&Choice::NoToolchain).unwrap_err();
    assert!(matches!(&err, Error::ToolchainUnset { tool } if tool == "cargo"));
    assert_eq!(
        err.remedy().map(|r| r.cause),
        Some(ap_core::remedy::Cause::ToolchainUnset {
            tool: "cargo".into()
        })
    );
    assert!(no_toolchain_error(&Choice::PlainCargo).is_ok());
    assert!(no_toolchain_error(&pick(None, &[STABLE], None)).is_ok());
}
