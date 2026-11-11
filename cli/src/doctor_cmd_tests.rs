//! The doctor's option parsing and mapping: the flags through the real clap
//! parse, the `Needs` and optional services of each kind, the heading, the
//! integration of the rows with a fake probe, and the guard on the commands a
//! default run may start. The probe is injected: nothing real is run, and no
//! project, work directory or network is touched. The rendering is in
//! `doctor_render_tests`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use ap_build::envcheck::{self, Needs, Outcome, Output, Probe, Required};
use ap_core::remedy::Cause;
use clap::{CommandFactory, Parser};

use crate::Cli;
use crate::Commands;
use crate::doctor_cmd::{
    DoctorArgs, check, exit_code, heading, json_report, needs_for, optional_services,
};

const SUSE: &str = "ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n";

fn doctor(argv: &[&str]) -> DoctorArgs {
    match Cli::try_parse_from(argv).unwrap().command {
        Commands::Doctor(args) => args,
        _ => panic!("not the doctor command: {argv:?}"),
    }
}

fn needs(argv: &[&str]) -> Needs {
    needs_for(&doctor(argv))
}

#[test]
fn every_kind_maps_to_its_needs() {
    // The default and `all` check Rust, local and the agent; go and npm only
    // as optional rows (see `optional_services_are_the_default_go_and_npm`).
    let default = Needs {
        agent: true,
        local: true,
        services: vec!["cargo_vendor".to_string()],
        rust: true,
        online: false,
    };
    assert_eq!(needs(&["auto-package", "doctor"]), default);
    assert_eq!(needs(&["auto-package", "doctor", "--for", "all"]), default);
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "rust"]),
        Needs {
            agent: true,
            rust: true,
            services: vec!["cargo_vendor".to_string()],
            ..Default::default()
        }
    );
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "go"]),
        Needs {
            agent: true,
            services: vec!["go_modules".to_string()],
            ..Default::default()
        }
    );
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "npm"]),
        Needs {
            agent: true,
            services: vec!["node_modules".to_string()],
            ..Default::default()
        }
    );
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "python"]),
        Needs {
            agent: true,
            ..Default::default()
        }
    );
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "local"]),
        Needs {
            agent: true,
            local: true,
            ..Default::default()
        }
    );
}

#[test]
fn optional_services_are_the_default_go_and_npm() {
    let optional = |argv: &[&str]| optional_services(&doctor(argv));
    assert_eq!(
        optional(&["auto-package", "doctor"]),
        ["go_modules", "node_modules"]
    );
    assert_eq!(
        optional(&["auto-package", "doctor", "--for", "all"]),
        ["go_modules", "node_modules"]
    );
    // Named, they are required (in `needs`) and no longer optional.
    assert!(optional(&["auto-package", "doctor", "--for", "go"]).is_empty());
    assert!(optional(&["auto-package", "doctor", "--for", "npm"]).is_empty());
    assert!(optional(&["auto-package", "doctor", "--for", "all,go"]) == ["node_modules"]);
    assert!(optional(&["auto-package", "doctor", "--for", "rust"]).is_empty());
}

#[test]
fn the_heading_says_the_mode_and_the_kinds() {
    assert_eq!(
        heading(&doctor(&["auto-package", "doctor"])),
        "auto-package doctor: environment check (offline)"
    );
    assert_eq!(
        heading(&doctor(&["auto-package", "doctor", "--online"])),
        "auto-package doctor: environment check (online)"
    );
    assert_eq!(
        heading(&doctor(&["auto-package", "doctor", "--for", "rust,local"])),
        "auto-package doctor: environment check (offline, for rust, local)"
    );
    assert_eq!(
        heading(&doctor(&[
            "auto-package",
            "doctor",
            "--online",
            "--for",
            "rust",
            "--for",
            "rust"
        ])),
        "auto-package doctor: environment check (online, for rust)"
    );
}

#[test]
fn kinds_combine_by_comma_or_by_repeating_the_flag() {
    let comma = needs(&["auto-package", "doctor", "--for", "rust,go"]);
    let repeated = needs(&["auto-package", "doctor", "--for", "rust", "--for", "go"]);
    assert_eq!(comma, repeated);
    assert_eq!(
        comma.services,
        vec!["cargo_vendor".to_string(), "go_modules".to_string()]
    );
    assert!(comma.rust && !comma.local);
    assert_eq!(
        needs(&["auto-package", "doctor", "--for", "python,all"]),
        needs(&["auto-package", "doctor"])
    );
}

#[test]
fn no_agent_and_online_set_their_flags() {
    let n = needs(&["auto-package", "doctor", "--no-agent", "--online"]);
    assert!(!n.agent && n.online);
    assert!(needs(&["auto-package", "doctor"]).agent);
    assert!(!needs(&["auto-package", "doctor"]).online);
}

#[test]
fn the_flags_parse_and_no_project_is_needed() {
    let args = doctor(&["auto-package", "doctor", "--json", "--online", "--no-agent"]);
    assert!(args.json && args.online && args.no_agent);
    assert!(Cli::try_parse_from(["auto-package", "doctor", "--project", "x"]).is_err());
    assert!(Cli::command().find_subcommand("doctor").is_some());
}

#[test]
fn an_unknown_kind_is_a_usage_error() {
    let err = Cli::try_parse_from(["auto-package", "doctor", "--for", "cargo"])
        .err()
        .expect("cargo is not a kind");
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

type Run<'a> = &'a dyn Fn(&[&str], Duration) -> Result<Output, String>;

fn fake_probe<'a>(path: OsString, run: Run<'a>) -> Probe<'a> {
    Probe {
        path,
        services_dir: PathBuf::from("/nonexistent/services"),
        os_release: SUSE.to_string(),
        oscrc_paths: Vec::new(),
        gh_hosts: Vec::new(),
        rustup_vars: Default::default(),
        uid: 1000,
        cargo_is_rustup: false,
        sandbox: Ok(()),
        build_root: Box::new(|| Ok((50 << 30, PathBuf::from("/var/tmp")))),
        env: BTreeMap::new(),
        run,
    }
}

/// Runs the checks for `argv` against a probe whose PATH holds a `claude`
/// stub and whose command runner records every call. Returns the calls.
fn recorded_calls(argv: &[&str]) -> Vec<Vec<String>> {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    fs::write(&claude, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    let calls = RefCell::new(Vec::<Vec<String>>::new());
    let record = |argv: &[&str], _: Duration| -> Result<Output, String> {
        calls
            .borrow_mut()
            .push(argv.iter().map(|s| s.to_string()).collect());
        Err("this test runs no command".to_string())
    };
    let probe = fake_probe(std::env::join_paths([dir.path()]).unwrap(), &record);
    let _ = check(&doctor(argv), &probe);
    calls.into_inner()
}

#[test]
fn a_default_run_never_asks_osc_whois_or_runs_claude_p() {
    let calls = recorded_calls(&["auto-package", "doctor"]);
    assert!(calls.contains(&vec![
        "claude".to_string(),
        "auth".to_string(),
        "status".to_string()
    ]));
    for call in &calls {
        let argv: Vec<&str> = call.iter().map(String::as_str).collect();
        assert!(
            envcheck::ALLOWED_COMMANDS.contains(&argv.as_slice()),
            "not an allowed command: {argv:?}"
        );
        assert!(!argv.contains(&"-p"));
        assert!(argv != ["osc", "whois"]);
    }
    // The positive control: --online is the only way to reach OBS.
    let online = recorded_calls(&["auto-package", "doctor", "--online"]);
    assert!(online.contains(&vec!["osc".to_string(), "whois".to_string()]));
}

#[test]
fn the_default_run_lists_go_and_npm_as_optional_services() {
    let services = tempfile::tempdir().unwrap();
    let run = |_: &[&str], _: Duration| -> Result<Output, String> { Err("no command".to_string()) };
    let mut probe = fake_probe(OsString::new(), &run);
    probe.services_dir = services.path().to_path_buf();
    let rows = check(&doctor(&["auto-package", "doctor"]), &probe);
    for id in ["service:go_modules", "service:node_modules"] {
        let row = rows.iter().find(|r| r.id == id).expect(id);
        assert_eq!(row.required, Required::No);
        assert_eq!(
            row.outcome,
            Outcome::WarnText("not installed; only needed for Go and npm packages".to_string())
        );
        assert_eq!(
            row.hint.as_deref(),
            Some(format!("sudo zypper install obs-service-{}", &id[8..]).as_str())
        );
    }
    let rows = check(&doctor(&["auto-package", "doctor", "--for", "go"]), &probe);
    let go = rows.iter().find(|r| r.id == "service:go_modules").unwrap();
    assert_eq!(go.required, Required::Yes);
    assert!(matches!(go.outcome, Outcome::Missing(_)));
}

#[test]
fn a_rust_only_host_with_go_and_npm_missing_is_ready() {
    let services = tempfile::tempdir().unwrap();
    let helper = services.path().join("cargo_vendor");
    fs::write(&helper, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |_: &[&str], _: Duration| -> Result<Output, String> { Err("no command".to_string()) };
    let mut probe = fake_probe(OsString::new(), &run);
    probe.services_dir = services.path().to_path_buf();
    let mut rows = envcheck::run(
        &Needs {
            services: vec!["cargo_vendor".to_string()],
            ..Default::default()
        },
        &probe,
    );
    rows.extend(envcheck::optional_service_rows(
        &probe,
        &["go_modules", "node_modules"],
        "only needed for Go and npm packages",
    ));
    // The rows of a Rust-only host: the service rows (the other rows are the
    // host's own, which this probe does not model).
    rows.retain(|r| r.id.starts_with("service:"));
    assert_eq!(rows.len(), 3);
    assert_eq!(exit_code(&rows), 0);
    assert!(json_report(&rows).ready);
}

#[test]
fn a_missing_bwrap_gets_its_package_in_the_install_line() {
    let services = tempfile::tempdir().unwrap();
    let run = |_: &[&str], _: Duration| -> Result<Output, String> { Err("no command".to_string()) };
    let mut probe = fake_probe(OsString::new(), &run);
    probe.services_dir = services.path().to_path_buf();
    probe.sandbox = Err(Cause::MissingTool {
        tool: "bwrap".to_string(),
        path: None,
    });
    let rows = check(
        &doctor(&["auto-package", "doctor", "--for", "rust"]),
        &probe,
    );
    let sandbox = rows.iter().find(|r| r.id == "sandbox").unwrap();
    assert_eq!(
        sandbox.hint.as_deref(),
        Some("sudo zypper install bubblewrap")
    );
    assert_eq!(sandbox.package, Some("bubblewrap"));
    let zypper = json_report(&rows).zypper.unwrap();
    assert!(
        zypper.split_whitespace().any(|w| w == "bubblewrap"),
        "{zypper}"
    );
}

#[test]
fn a_missing_claude_says_how_to_install_and_log_in() {
    let services = tempfile::tempdir().unwrap();
    let run = |_: &[&str], _: Duration| -> Result<Output, String> { Err("no command".to_string()) };
    let mut probe = fake_probe(OsString::new(), &run);
    probe.services_dir = services.path().to_path_buf();
    let rows = check(&doctor(&["auto-package", "doctor"]), &probe);
    let claude = rows.iter().find(|r| r.id == "claude").unwrap();
    assert!(matches!(
        &claude.outcome,
        Outcome::MissingText(t) if t.contains("https://claude.com/claude-code") && t.contains("claude auth login")
    ));
    assert_eq!(claude.hint, None);
    assert_eq!(claude.package, None);
}

#[test]
fn a_missing_gcc_or_cargo_names_its_package() {
    let services = tempfile::tempdir().unwrap();
    let run = |_: &[&str], _: Duration| -> Result<Output, String> { Err("no command".to_string()) };
    let mut probe = fake_probe(OsString::new(), &run);
    probe.services_dir = services.path().to_path_buf();
    let rows = check(&doctor(&["auto-package", "doctor"]), &probe);
    let cc = rows.iter().find(|r| r.id == "cc").unwrap();
    assert_eq!(cc.hint.as_deref(), Some("sudo zypper install gcc"));
    let rows = check(
        &doctor(&["auto-package", "doctor", "--for", "rust"]),
        &probe,
    );
    let cargo = rows.iter().find(|r| r.id == "cargo").unwrap();
    assert_eq!(cargo.hint.as_deref(), Some("sudo zypper install rustup"));
    assert_eq!(cargo.package, Some("rustup"));
}
