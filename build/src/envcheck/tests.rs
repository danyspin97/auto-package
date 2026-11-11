//! The checks against injected facts: a fake PATH of stubs, a fake services
//! directory, temporary oscrc and rustup homes, and a scripted `run`. No
//! network, no model, no real package or service is touched.

use super::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::agent::NOT_LOGGED_IN_MESSAGE;

const SUSE: &str = "ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n";
const UBUNTU: &str = "ID=ubuntu\nID_LIKE=debian\n";

pub(super) type Run<'a> = &'a dyn Fn(&[&str], Duration) -> Result<Output, String>;

pub(super) fn ok_output(stdout: &str) -> Output {
    Output {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

pub(super) fn no_run(_: &[&str], _: Duration) -> Result<Output, String> {
    Err("this test runs no command".to_string())
}

pub(super) fn triple() -> String {
    format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
}

/// A directory holding an executable stub for each of `names`.
pub(super) fn stubs(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let path = dir.path().join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

pub(super) fn path_of(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}

/// A probe where every fact is healthy, unless a test changes it. Rustup is
/// not involved by default, so the rustup rows are hidden.
pub(super) fn probe<'a>(path: OsString, services: &Path, run: Run<'a>) -> Probe<'a> {
    Probe {
        path,
        services_dir: services.to_path_buf(),
        os_release: SUSE.to_string(),
        oscrc_paths: Vec::new(),
        gh_hosts: Vec::new(),
        rustup_vars: HostVars::default(),
        // SAFETY: geteuid has no preconditions and cannot fail.
        uid: unsafe { libc::geteuid() },
        cargo_is_rustup: false,
        sandbox: Ok(()),
        build_root: Box::new(|| Ok((50 << 30, PathBuf::from("/var/tmp")))),
        env: BTreeMap::new(),
        run,
    }
}

pub(super) fn missing_tool(tool: &str) -> Cause {
    Cause::MissingTool {
        tool: tool.to_string(),
        path: None,
    }
}

pub(super) fn row<'r>(rows: &'r [CheckResult], id: &str) -> &'r CheckResult {
    rows.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no row {id} in {:?}", ids(rows)))
}

pub(super) fn ids(rows: &[CheckResult]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_str()).collect()
}

fn applies(id: &str, needs: &Needs) -> bool {
    let check = CHECKS.iter().find(|c| c.id == id).unwrap();
    (check.applies)(needs)
}

#[test]
fn every_check_has_a_unique_id_and_a_name() {
    let mut seen = HashSet::new();
    for check in CHECKS {
        assert!(!check.id.is_empty() && !check.name.is_empty());
        assert!(seen.insert(check.id), "duplicate id {}", check.id);
    }
}

#[test]
fn applies_follows_the_needs() {
    let none = Needs::default();
    for id in ["osc", "oscrc", "rpmspec", "quilt", "sandbox", "gpg"] {
        assert!(applies(id, &none), "{id} is for every run");
    }
    for id in [
        "claude",
        "build",
        "sudo",
        "build-root",
        "cc",
        "cargo",
        "rustup-default",
        "sandbox-toolchain",
        "osc-whois",
    ] {
        assert!(!applies(id, &none), "{id} is not for a plain run");
    }
    let all = Needs {
        agent: true,
        local: true,
        services: vec!["cargo_vendor".to_string()],
        rust: true,
        online: true,
    };
    assert!(applies("claude", &all) && applies("build", &all));
    assert!(applies("sudo", &all) && applies("build-root", &all) && applies("cc", &all));
    assert!(applies("cargo", &all) && applies("rustup-default", &all));
    assert!(applies("sandbox-toolchain", &all) && applies("osc-whois", &all));
    assert!(!applies(
        "claude",
        &Needs {
            agent: false,
            ..all.clone()
        }
    ));
}

#[test]
fn an_oscrc_is_found_by_its_file() {
    let services = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join("oscrc");
    let missing = dir.path().join("none");
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.oscrc_paths = vec![missing.clone()];
    let rows = run(&Needs::default(), &p);
    let found = &row(&rows, "oscrc").outcome;
    assert!(
        matches!(found, Outcome::MissingText(t) if t.contains("none")),
        "{found:?}"
    );
    assert_eq!(row(&rows, "oscrc").required, Required::Yes);

    fs::write(&rc, "[general]\n").unwrap();
    p.oscrc_paths = vec![missing, rc.clone()];
    let rows = run(&Needs::default(), &p);
    assert!(matches!(&row(&rows, "oscrc").outcome, Outcome::Ok(t) if t.contains("oscrc")));
}

#[test]
fn a_missing_tool_gets_its_package_on_suse_and_the_generic_hint_elsewhere() {
    let services = tempfile::tempdir().unwrap();
    let only_osc = stubs(&["osc"]);
    let path = path_of(&[only_osc.path()]);
    let mut p = probe(path, services.path(), &no_run);

    let rows = run(&Needs::default(), &p);
    assert!(matches!(row(&rows, "osc").outcome, Outcome::Ok(_)));
    let quilt = row(&rows, "quilt");
    assert!(matches!(quilt.outcome, Outcome::Missing(_)));
    assert_eq!(quilt.hint.as_deref(), Some("sudo zypper install quilt"));
    assert_eq!(row(&rows, "rpmspec").package, Some("rpm-build"));

    p.os_release = UBUNTU.to_string();
    let rows = run(&Needs::default(), &p);
    let quilt = row(&rows, "quilt");
    assert_eq!(
        quilt.hint.as_deref(),
        Some("install the package that provides quilt")
    );
    assert_eq!(quilt.package, None);
}

#[test]
fn a_service_is_checked_by_its_helper_and_named_once() {
    let services = tempfile::tempdir().unwrap();
    let helper = services.path().join("cargo_vendor");
    fs::write(&helper, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let needs = Needs {
        services: vec![
            "tar_scm".to_string(),
            "cargo_vendor".to_string(),
            "tar_scm".to_string(),
        ],
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    let service_rows: Vec<&str> = rows
        .iter()
        .filter(|r| r.id.starts_with("service:"))
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(service_rows, ["service:cargo_vendor", "service:tar_scm"]);
    assert!(matches!(
        row(&rows, "service:cargo_vendor").outcome,
        Outcome::Ok(_)
    ));
    let tar = row(&rows, "service:tar_scm");
    assert_eq!(tar.required, Required::Yes);
    assert_eq!(
        tar.hint.as_deref(),
        Some("sudo zypper install obs-service-obs_scm-common")
    );
    assert_eq!(tar.package, Some("obs-service-obs_scm-common"));
}

#[test]
fn a_service_with_no_package_is_searched_for_by_its_path() {
    let services = tempfile::tempdir().unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let needs = Needs {
        services: vec!["new_service".to_string()],
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    let hint = row(&rows, "service:new_service").hint.clone().unwrap();
    assert!(hint.starts_with("zypper se -f "), "{hint}");
    assert_eq!(row(&rows, "service:new_service").package, None);
}

#[test]
fn claude_logged_out_is_missing_and_logged_in_is_not() {
    let services = tempfile::tempdir().unwrap();
    let claude = stubs(&["claude"]);
    let path = path_of(&[claude.path()]);
    let needs = Needs {
        agent: true,
        ..Needs::default()
    };
    let out_logged_out = |argv: &[&str], _: Duration| {
        assert_eq!(argv, ["claude", "auth", "status"]);
        Ok(ok_output("{\"loggedIn\": false}"))
    };
    let p = probe(path.clone(), services.path(), &out_logged_out);
    let rows = run(&needs, &p);
    assert_eq!(
        row(&rows, "claude").outcome,
        Outcome::MissingText(NOT_LOGGED_IN_MESSAGE.to_string())
    );

    let logged_in = |_: &[&str], _: Duration| Ok(ok_output("{\"loggedIn\": true}"));
    let p = probe(path, services.path(), &logged_in);
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "claude").outcome, Outcome::Ok(_)));
}

#[test]
fn claude_missing_from_the_path_is_missing_without_a_run() {
    let services = tempfile::tempdir().unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let needs = Needs {
        agent: true,
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    assert!(matches!(
        &row(&rows, "claude").outcome,
        Outcome::Missing(Cause::MissingTool { tool, .. }) if tool == "claude"
    ));
}

#[test]
fn the_build_root_is_the_probe_fact() {
    let services = tempfile::tempdir().unwrap();
    let needs = Needs {
        local: true,
        ..Needs::default()
    };
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.build_root = Box::new(|| Err("not enough free space".to_string()));
    let rows = run(&needs, &p);
    assert_eq!(
        row(&rows, "build-root").outcome,
        Outcome::MissingText("not enough free space".to_string())
    );
    let p = probe(OsString::new(), services.path(), &no_run);
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "build-root").outcome, Outcome::Ok(_)));
}

#[test]
fn the_sandbox_is_missing_with_its_reason_and_warned_when_unsafe() {
    let services = tempfile::tempdir().unwrap();
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.sandbox = Err("bwrap is not installed".to_string());
    let rows = run(&Needs::default(), &p);
    assert_eq!(
        row(&rows, "sandbox").outcome,
        Outcome::Missing(Cause::SandboxUnavailable {
            why: "bwrap is not installed".to_string()
        })
    );

    p.env.insert(UNSAFE_ENV.to_string(), "1".to_string());
    let rows = run(&Needs::default(), &p);
    assert!(matches!(
        &row(&rows, "sandbox").outcome,
        Outcome::WarnText(t) if t.contains(UNSAFE_ENV)
    ));

    p.sandbox = Ok(());
    p.env.insert(UNSAFE_ENV.to_string(), "0".to_string());
    let rows = run(&Needs::default(), &p);
    assert!(matches!(row(&rows, "sandbox").outcome, Outcome::Ok(_)));
}

#[test]
fn optional_rows_are_never_missing() {
    let services = tempfile::tempdir().unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let needs = Needs {
        local: true,
        online: false,
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    for r in rows.iter().filter(|r| r.required == Required::No) {
        assert!(
            !matches!(r.outcome, Outcome::Missing(_) | Outcome::MissingText(_)),
            "{}: {:?}",
            r.id,
            r.outcome
        );
    }
    assert!(matches!(row(&rows, "gpg").outcome, Outcome::Warn(_)));
    assert!(matches!(
        row(&rows, "github-token").outcome,
        Outcome::WarnText(_)
    ));
    assert!(matches!(
        row(&rows, "gitlab-token").outcome,
        Outcome::WarnText(_)
    ));
    assert!(matches!(row(&rows, "cc").outcome, Outcome::Warn(_)));
}

#[test]
fn a_token_in_the_environment_or_a_gh_login_quiets_the_github_warning() {
    let services = tempfile::tempdir().unwrap();
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.env.insert("GH_TOKEN".to_string(), "x".to_string());
    let rows = run(&Needs::default(), &p);
    assert!(matches!(row(&rows, "github-token").outcome, Outcome::Ok(_)));

    p.env.clear();
    let login = tempfile::tempdir().unwrap();
    let hosts = login.path().join("hosts.yml");
    fs::write(&hosts, "github.com:\n").unwrap();
    p.gh_hosts = vec![hosts];
    let rows = run(&Needs::default(), &p);
    assert!(matches!(row(&rows, "github-token").outcome, Outcome::Ok(_)));
}

#[test]
fn osc_whois_runs_only_for_an_online_run() {
    let services = tempfile::tempdir().unwrap();
    let calls = RefCell::new(Vec::<Vec<String>>::new());
    let record = |argv: &[&str], _: Duration| {
        calls
            .borrow_mut()
            .push(argv.iter().map(|s| s.to_string()).collect());
        Ok(ok_output("account ok"))
    };
    let p = probe(OsString::new(), services.path(), &record);
    let offline = Needs {
        agent: true,
        local: true,
        services: vec!["cargo_vendor".to_string()],
        rust: true,
        online: false,
    };
    run(&offline, &p);
    assert!(calls.borrow().iter().all(|c| c != &["osc", "whois"]));

    let rows = run(
        &Needs {
            online: true,
            ..offline
        },
        &p,
    );
    assert!(calls.borrow().iter().any(|c| c == &["osc", "whois"]));
    assert!(matches!(row(&rows, "osc-whois").outcome, Outcome::Ok(_)));
}

#[test]
fn the_run_closure_is_only_given_allowed_commands() {
    let services = tempfile::tempdir().unwrap();
    let calls = RefCell::new(Vec::<Vec<String>>::new());
    let record = |argv: &[&str], _: Duration| {
        calls
            .borrow_mut()
            .push(argv.iter().map(|s| s.to_string()).collect());
        Ok(ok_output("{\"loggedIn\": true}"))
    };
    let claude = stubs(&["claude"]);
    let p = probe(path_of(&[claude.path()]), services.path(), &record);
    for online in [false, true] {
        let needs = Needs {
            agent: true,
            local: true,
            services: vec!["cargo_vendor".to_string()],
            rust: true,
            online,
        };
        run(&needs, &p);
    }
    let calls = calls.into_inner();
    assert!(!calls.is_empty());
    for call in &calls {
        let argv: Vec<&str> = call.iter().map(String::as_str).collect();
        assert!(
            ALLOWED_COMMANDS
                .iter()
                .any(|allowed| *allowed == argv.as_slice()),
            "not an allowed command: {argv:?}"
        );
    }
    assert!(!calls.iter().any(|c| c.contains(&"-p".to_string())));
    assert!(ALLOWED_COMMANDS.iter().all(|c| !c.contains(&"-p")));
    assert!(calls.iter().any(|c| c == &["claude", "auth", "status"]));
}

#[test]
fn the_zypper_line_names_each_missing_package_once_in_order() {
    let result = |id: &str, outcome: Outcome, package: Option<&'static str>| CheckResult {
        id: id.to_string(),
        name: id.to_string(),
        required: Required::Yes,
        outcome,
        hint: None,
        package,
    };
    let missing = |tool: &str| Outcome::Missing(missing_tool(tool));
    let rows = vec![
        result("osc", missing("osc"), Some("osc")),
        result("quilt", missing("quilt"), Some("quilt")),
        result("again", missing("osc"), Some("osc")),
        result(
            "service:cargo_vendor",
            missing("cargo_vendor"),
            Some("obs-service-cargo"),
        ),
        result("rpmspec", missing("rpmspec"), Some("rpm-build")),
        // A warning is not installed by the line, nor a row without a package.
        result("gpg", Outcome::Warn(missing_tool("gpg")), Some("gpg2")),
        result("claude", Outcome::MissingText("x".to_string()), None),
    ];
    assert_eq!(
        zypper_line(&rows).as_deref(),
        Some("sudo zypper install obs-service-cargo osc quilt rpm-build")
    );
    assert_eq!(zypper_line(&rows[5..]), None);
}

#[test]
fn the_zypper_line_is_none_off_suse_and_when_nothing_is_missing() {
    let services = tempfile::tempdir().unwrap();
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.os_release = UBUNTU.to_string();
    let rows = run(&Needs::default(), &p);
    assert!(
        rows.iter()
            .any(|r| matches!(r.outcome, Outcome::Missing(_)))
    );
    assert_eq!(zypper_line(&rows), None);

    let all = stubs(&["osc", "rpmspec", "quilt", "gpg", "cc", "gcc"]);
    let p = probe(path_of(&[all.path()]), services.path(), &no_run);
    let rows = run(&Needs::default(), &p);
    assert_eq!(zypper_line(&rows), None);
}

#[test]
fn a_run_that_names_no_service_has_no_service_rows() {
    let services = tempfile::tempdir().unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let rows = run(&Needs::default(), &p);
    assert_eq!(
        ids(&rows),
        [
            "osc",
            "oscrc",
            "rpmspec",
            "quilt",
            "sandbox",
            "github-token",
            "gitlab-token",
            "gpg"
        ]
    );
}
