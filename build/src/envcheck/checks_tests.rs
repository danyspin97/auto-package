//! The claude, sudo, service, build-root, oscrc and command-runner checks
//! against injected facts and scripted commands. Nothing real is run: a
//! closure that must not be called panics, and `host_run` refuses before it
//! spawns anything.

use super::tests::{missing_tool, no_run, ok_output, path_of, probe, row, stubs};
use super::*;
use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

fn claude_stub() -> tempfile::TempDir {
    stubs(&["claude"])
}

#[test]
fn claude_that_times_out_or_cannot_launch_is_a_warning_not_a_missing_login() {
    let services = tempfile::tempdir().unwrap();
    let claude = claude_stub();
    let path = path_of(&[claude.path()]);
    let needs = Needs {
        agent: true,
        ..Needs::default()
    };
    let seen = RefCell::new(None);
    let timed_out = |_: &[&str], timeout: Duration| {
        *seen.borrow_mut() = Some(timeout);
        Err("claude did not finish within 30s and was killed".to_string())
    };
    let p = probe(path.clone(), services.path(), &timed_out);
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "claude").outcome, Outcome::WarnText(_)));
    assert_eq!(*seen.borrow(), Some(Duration::from_secs(30)));

    let launch_failed = |_: &[&str], _: Duration| Err("could not launch claude".to_string());
    let p = probe(path.clone(), services.path(), &launch_failed);
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "claude").outcome, Outcome::WarnText(_)));
}

#[test]
fn claude_with_odd_status_output_is_not_a_logout() {
    let services = tempfile::tempdir().unwrap();
    let claude = claude_stub();
    let needs = Needs {
        agent: true,
        ..Needs::default()
    };
    let odd = |_: &[&str], _: Duration| Ok(ok_output("status: something new"));
    let p = probe(path_of(&[claude.path()]), services.path(), &odd);
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "claude").outcome, Outcome::Ok(_)));
}

#[test]
fn claude_runs_with_the_agent_environment_and_others_with_a_short_one() {
    assert_eq!(command_env("claude"), crate::agent::agent_env());
    let short = command_env("osc");
    assert!(short.keys().all(|k| {
        matches!(
            k.as_str(),
            "PATH"
                | "HOME"
                | "LANG"
                | "XDG_CONFIG_HOME"
                | "XDG_RUNTIME_DIR"
                | "DBUS_SESSION_BUS_ADDRESS"
                | "OSC_CONFIG"
        )
    }));
}

#[test]
fn host_run_refuses_a_command_outside_the_allowed_list_without_spawning() {
    // `true` would succeed if it ran; the refusal is an Err, not an Output.
    assert!(host_run(&["true"], Duration::from_secs(5)).is_err());
    assert!(host_run(&["claude", "-p", "hi"], Duration::from_secs(5)).is_err());
    assert!(host_run(&["sudo", "-n", "true"], Duration::from_secs(5)).is_err());
    assert!(host_run(&[], Duration::from_secs(5)).is_err());
}

#[test]
fn sudo_that_allows_osc_build_is_ok_and_asks_only_that() {
    let services = tempfile::tempdir().unwrap();
    let needs = Needs {
        local: true,
        ..Needs::default()
    };
    let asked = RefCell::new(Vec::<String>::new());
    let allowed = |argv: &[&str], _: Duration| {
        asked.borrow_mut().push(argv.join(" "));
        Ok(ok_output(""))
    };
    let mut p = probe(OsString::new(), services.path(), &allowed);
    p.uid = 1000;
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "sudo").outcome, Outcome::Ok(_)));
    assert_eq!(*asked.borrow(), ["sudo -n -l osc build"]);
}

#[test]
fn sudo_that_refuses_osc_build_is_missing_for_a_local_build() {
    let services = tempfile::tempdir().unwrap();
    let needs = Needs {
        local: true,
        ..Needs::default()
    };
    let refused = |_: &[&str], _: Duration| {
        Ok(Output {
            success: false,
            stdout: String::new(),
            stderr: "sudo: a password is required".to_string(),
        })
    };
    let mut p = probe(OsString::new(), services.path(), &refused);
    p.uid = 1000;
    let rows = run(&needs, &p);
    assert!(matches!(
        &row(&rows, "sudo").outcome,
        Outcome::MissingText(t) if t.contains("NOPASSWD")
    ));
    assert_eq!(row(&rows, "sudo").required, Required::Yes);
}

#[test]
fn sudo_that_cannot_be_asked_is_a_warning() {
    let services = tempfile::tempdir().unwrap();
    let needs = Needs {
        local: true,
        ..Needs::default()
    };
    let broken = |_: &[&str], _: Duration| Err("sudo timed out".to_string());
    let mut p = probe(OsString::new(), services.path(), &broken);
    p.uid = 1000;
    let rows = run(&needs, &p);
    assert!(matches!(row(&rows, "sudo").outcome, Outcome::WarnText(_)));
}

#[test]
fn root_needs_no_sudo_row_and_no_sudo_command() {
    let services = tempfile::tempdir().unwrap();
    let needs = Needs {
        local: true,
        ..Needs::default()
    };
    let never = |argv: &[&str], _: Duration| -> Result<Output, String> {
        panic!("root must not run {argv:?}")
    };
    let mut p = probe(OsString::new(), services.path(), &never);
    p.uid = 0;
    let rows = run(&needs, &p);
    assert!(rows.iter().all(|r| r.id != "sudo"));
}

#[test]
fn invalid_service_names_are_rejected_and_shown_printable() {
    let services = tempfile::tempdir().unwrap();
    let names = [
        ".",
        "..",
        "/etc/passwd",
        "../../etc/passwd",
        "",
        "a\u{1b}[31mb",
    ];
    let p = probe(OsString::new(), services.path(), &no_run);
    for name in names {
        let needs = Needs {
            services: vec![name.to_string()],
            ..Needs::default()
        };
        let rows = run(&needs, &p);
        let service_row = rows
            .iter()
            .find(|r| r.id.starts_with("service:"))
            .unwrap_or_else(|| panic!("no service row for {name:?}"));
        assert!(
            !service_row.id.chars().any(char::is_control),
            "{:?}",
            service_row.id
        );
        assert!(
            !service_row.name.chars().any(char::is_control),
            "{:?}",
            service_row.name
        );
        assert!(
            matches!(&service_row.outcome, Outcome::MissingText(t) if t.starts_with("invalid service name")),
            "{name:?}: {:?}",
            service_row.outcome
        );
    }
    let needs = Needs {
        services: vec!["a\u{1b}[31mb".to_string()],
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    assert_eq!(row(&rows, "service:a?[31mb").name, "OBS service a?[31mb");
}

#[test]
fn a_service_must_be_an_executable_file_not_just_present() {
    let services = tempfile::tempdir().unwrap();
    let plain = services.path().join("plain");
    fs::write(&plain, "text").unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
    let tool = services.path().join("tool");
    fs::write(&tool, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(OsString::new(), services.path(), &no_run);
    let needs = Needs {
        services: vec![
            "plain".to_string(),
            "tool".to_string(),
            "absent".to_string(),
        ],
        ..Needs::default()
    };
    let rows = run(&needs, &p);
    assert!(matches!(
        &row(&rows, "service:plain").outcome,
        Outcome::Missing(Cause::MissingTool { tool, .. }) if tool == "plain"
    ));
    assert!(matches!(row(&rows, "service:tool").outcome, Outcome::Ok(_)));
    assert!(matches!(
        row(&rows, "service:absent").outcome,
        Outcome::Missing(_)
    ));
}

#[test]
fn a_non_local_run_never_reads_the_build_root() {
    let services = tempfile::tempdir().unwrap();
    let calls = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&calls);
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.build_root = Box::new(move || {
        counter.set(counter.get() + 1);
        Ok((1 << 40, PathBuf::from("/var/tmp")))
    });
    let rows = run(&Needs::default(), &p);
    assert_eq!(calls.get(), 0);
    assert!(rows.iter().all(|r| r.id != "build-root"));

    let local = Needs {
        local: true,
        ..Needs::default()
    };
    let mut p = probe(OsString::new(), services.path(), &no_run);
    let counter = Rc::clone(&calls);
    p.uid = 1000;
    p.build_root = Box::new(move || {
        counter.set(counter.get() + 1);
        Ok((1 << 40, PathBuf::from("/var/tmp")))
    });
    run(&local, &p);
    assert_eq!(calls.get(), 1);
}

#[test]
fn osc_config_alone_counts_when_it_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let home_rc = dir.path().join("oscrc");
    fs::write(&home_rc, "[general]\n").unwrap();
    let osc_config = dir.path().join("missing-config");
    let set = oscrc_candidates(vec![osc_config.clone(), home_rc.clone()], true);
    assert_eq!(set, [osc_config.clone()]);
    let services = tempfile::tempdir().unwrap();
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.oscrc_paths = set;
    let rows = run(&Needs::default(), &p);
    assert!(matches!(
        row(&rows, "oscrc").outcome,
        Outcome::MissingText(_)
    ));

    let unset = oscrc_candidates(vec![osc_config, home_rc.clone()], false);
    assert_eq!(unset.len(), 2);
    p.oscrc_paths = unset;
    let rows = run(&Needs::default(), &p);
    assert!(matches!(row(&rows, "oscrc").outcome, Outcome::Ok(_)));
}

#[test]
fn only_rustup_on_path_is_a_warning_that_says_so() {
    let services = tempfile::tempdir().unwrap();
    let rustup = stubs(&["rustup"]);
    let p = probe(path_of(&[rustup.path()]), services.path(), &no_run);
    let rows = run(
        &Needs {
            rust: true,
            ..Needs::default()
        },
        &p,
    );
    match &row(&rows, "cargo").outcome {
        Outcome::WarnText(text) => assert!(
            text.contains("rustup is on PATH but cargo is not"),
            "{text}"
        ),
        other => panic!("{other:?}"),
    }
    let neither = probe(OsString::new(), services.path(), &no_run);
    let rows = run(
        &Needs {
            rust: true,
            ..Needs::default()
        },
        &neither,
    );
    assert_eq!(
        row(&rows, "cargo").outcome,
        Outcome::Missing(missing_tool("cargo"))
    );
}

#[test]
fn an_unsafe_sandbox_warning_does_not_repeat_the_switch_in_its_reason() {
    let services = tempfile::tempdir().unwrap();
    let mut p = probe(OsString::new(), services.path(), &no_run);
    p.sandbox = Err(
        "bwrap is not installed. Install bubblewrap or set AUTO_PACKAGE_UNSAFE_SERVICE=1"
            .to_string(),
    );
    p.env.insert(UNSAFE_ENV.to_string(), "1".to_string());
    let rows = run(&Needs::default(), &p);
    match &row(&rows, "sandbox").outcome {
        Outcome::WarnText(text) => {
            assert_eq!(text.matches(UNSAFE_ENV).count(), 1, "{text}");
            assert!(!text.contains("bwrap is not installed"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}
