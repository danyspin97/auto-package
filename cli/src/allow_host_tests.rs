//! The `--allow-host` flag and the `hosts` subcommand: how they parse, and
//! what the helpers write and say. The file is always a temporary one.

use super::allow_host::*;
use super::*;
use ap_build::allowed_hosts::{AllowedHosts, Origin, Use, Via};
use clap::Parser;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(args)
}

#[test]
fn allow_host_repeats_on_build_update_and_lint() {
    let build = parse(&[
        "auto-package",
        "build",
        "foo",
        "--project",
        "p",
        "--allow-host",
        "a.example.org",
        "--allow-host",
        "b.example.org",
    ])
    .unwrap();
    let Commands::Build { allow_host, .. } = build.command else {
        panic!("not a build");
    };
    assert_eq!(allow_host.allow_host, ["a.example.org", "b.example.org"]);

    let lint = parse(&[
        "auto-package",
        "lint",
        "--project",
        "p",
        "--allow-host",
        "c.example.org",
    ])
    .unwrap();
    let Commands::Lint(args) = lint.command else {
        panic!("not a lint");
    };
    assert_eq!(args.allow_host.allow_host, ["c.example.org"]);

    let update = parse(&[
        "auto-package",
        "update",
        "--project",
        "p",
        "--allow-host",
        "d.example.org",
    ])
    .unwrap();
    let Commands::Update(args) = update.command else {
        panic!("not an update");
    };
    assert_eq!(args.allow_host.allow_host, ["d.example.org"]);
}

#[test]
fn hosts_subcommands_parse() {
    let list = parse(&["auto-package", "hosts", "list"]).unwrap();
    assert!(matches!(
        list.command,
        Commands::Hosts {
            command: HostsCommand::List
        }
    ));
    let allow = parse(&[
        "auto-package",
        "hosts",
        "allow",
        "mirror.example.org",
        "--reason",
        "our mirror",
    ])
    .unwrap();
    let Commands::Hosts {
        command: HostsCommand::Allow { host, reason },
    } = allow.command
    else {
        panic!("not allow");
    };
    assert_eq!(
        (host.as_str(), reason.as_deref()),
        ("mirror.example.org", Some("our mirror"))
    );
    let remove = parse(&["auto-package", "hosts", "remove", "mirror.example.org"]).unwrap();
    assert!(matches!(
        remove.command,
        Commands::Hosts {
            command: HostsCommand::Remove { .. }
        }
    ));
}

#[test]
fn a_host_argument_is_required_and_the_flag_needs_a_value() {
    assert!(parse(&["auto-package", "hosts", "allow"]).is_err());
    assert!(
        parse(&[
            "auto-package",
            "build",
            "foo",
            "--project",
            "p",
            "--allow-host"
        ])
        .is_err()
    );
}

#[test]
fn allow_writes_once_and_says_what_it_did() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("auto-package").join("allowed-hosts");
    let hosts = AllowedHosts::empty();
    let said = allow_in(&path, "Mirror.Example.org.", Some("our mirror"), &hosts).unwrap();
    assert!(
        said.contains("allowed mirror.example.org permanently"),
        "{said}"
    );
    let again = allow_in(&path, "mirror.example.org", None, &hosts).unwrap();
    assert!(again.contains("already in"), "{again}");
    // A host the built-in list covers is not written.
    let builtin = allow_in(&path, "github.com", None, &hosts).unwrap();
    assert!(
        builtin.contains("built-in") && builtin.contains("nothing was written"),
        "{builtin}"
    );
    assert!(allow_in(&path, "*.example.org", None, &hosts).is_err());
    assert!(allow_in(&path, "https://example.org", None, &hosts).is_err());
}

#[test]
fn remove_refuses_built_in_hosts_and_names_the_other_origins() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("allowed-hosts");
    std::fs::write(&path, "mirror.example.org  # ours\n").unwrap();
    let hosts = AllowedHosts::with_env("env.example.org");
    let err = remove_in(&path, "github.com", &hosts).unwrap_err();
    assert!(err.contains("built in and cannot be removed"), "{err}");
    let err = remove_in(&path, "env.example.org", &hosts).unwrap_err();
    assert!(err.contains("AUTO_PACKAGE_SOURCE_HOSTS"), "{err}");
    let err = remove_in(&path, "nothing.example.org", &hosts).unwrap_err();
    assert!(err.contains("is not in"), "{err}");
    let ok = remove_in(&path, "mirror.example.org", &hosts).unwrap();
    assert!(ok.starts_with("removed mirror.example.org"), "{ok}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn the_list_shows_origin_reason_and_file_problems() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("allowed-hosts");
    std::fs::write(&path, "ok.example.org  # because\nbad line here!\n").unwrap();
    let hosts = ap_build::allowed_hosts::file::load(&path);
    let shared = AllowedHosts::with_env("env.example.org");
    for p in &hosts.problems {
        shared.add_problem(format!("{}: {p}", path.display()));
    }
    for e in hosts.entries {
        shared.allow(&e.host, Origin::File, e.reason).unwrap();
    }
    shared
        .allow("flag.example.org", Origin::Run(Via::Flag), None)
        .unwrap();
    let text = list_text(&shared, &path);
    assert!(text.contains("env.example.org") && text.contains("env AUTO_PACKAGE_SOURCE_HOSTS"));
    assert!(
        text.contains("ok.example.org") && text.contains("# because"),
        "{text}"
    );
    assert!(text.contains("--allow-host (this run)"), "{text}");
    // A user entry covers its subdomains and both uses; the text says so.
    assert!(
        text.contains("ok.example.org") && text.contains("services+sources, subdomains included"),
        "{text}"
    );
    assert!(
        text.contains("warning: ") && text.contains("line 2"),
        "{text}"
    );
    assert!(
        text.contains("github.com") && text.contains("built-in"),
        "{text}"
    );
}

#[test]
fn a_single_label_or_a_public_suffix_is_refused_by_hosts_allow_and_the_flag() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("allowed-hosts");
    let hosts = AllowedHosts::empty();
    for raw in ["com", "co.uk", "github.io"] {
        let err = allow_in(&path, raw, None, &hosts).unwrap_err();
        assert!(err.contains("`"), "{err}");
    }
    assert!(!path.exists(), "a refused name writes nothing");
}

#[test]
fn a_host_built_in_for_services_only_is_written_so_sources_can_use_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("allowed-hosts");
    let hosts = AllowedHosts::empty();
    let said = allow_in(&path, "proxy.golang.org", None, &hosts).unwrap();
    assert!(
        said.contains("allowed proxy.golang.org permanently"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "proxy.golang.org\n"
    );
}

#[test]
fn the_list_shows_no_control_characters_from_a_reason() {
    let path = std::path::Path::new("/nonexistent/allowed-hosts");
    let hosts = AllowedHosts::empty();
    hosts
        .allow(
            "mirror.example.org",
            Origin::File,
            Some("\x1b[2Jcleared".to_string()),
        )
        .unwrap();
    let text = list_text(&hosts, path);
    let shown = text
        .lines()
        .find(|l| l.starts_with("mirror.example.org"))
        .unwrap();
    assert!(!shown.chars().any(char::is_control), "{shown:?}");
}

#[test]
fn the_run_summary_names_each_refused_host_once_with_the_hint() {
    let hosts = AllowedHosts::empty();
    assert_eq!(refused_section(&hosts), "");
    for h in ["b.example.org", "a.example.org", "b.example.org"] {
        let _ = hosts.check_host(
            h,
            ap_build::allowed_hosts::Use::Service,
            &ap_build::allowed_hosts::Asker::service("cli-test", "test"),
        );
    }
    let text = refused_section(&hosts);
    assert!(text.contains("Hosts refused this run (2):"), "{text}");
    assert_eq!(
        text.matches("host a.example.org is not allowed").count(),
        1,
        "{text}"
    );
    assert!(text.contains("--allow-host b.example.org"), "{text}");
    assert!(
        text.contains("`auto-package hosts allow a.example.org`"),
        "{text}"
    );
    // A host allowed since (from the TUI, say) is no longer listed.
    hosts
        .allow("a.example.org", Origin::Run(Via::Tui), None)
        .unwrap();
    assert!(refused_section(&hosts).contains("(1)"));
}

#[test]
fn a_source_refusal_of_a_service_only_host_is_listed_for_sources() {
    let hosts = AllowedHosts::empty();
    let _ = hosts.check_host(
        "proxy.golang.org",
        Use::Source,
        &ap_build::allowed_hosts::Asker::source("cli-test"),
    );
    let text = refused_section(&hosts);
    assert!(text.contains("proxy.golang.org"), "{text}");
    assert!(text.contains("(for source fetches)"), "{text}");
    hosts
        .allow("proxy.golang.org", Origin::Run(Via::Tui), None)
        .unwrap();
    assert_eq!(refused_section(&hosts), "");
}

#[derive(Default)]
struct Events(std::sync::Mutex<Vec<ap_core::event::Event>>);

impl ap_core::sink::Reporter for Events {
    fn report(&self, event: &ap_core::event::Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn allowed_hosts_and_problems_are_reported_once_the_run_has_a_reporter() {
    use ap_core::event::Event;
    let args = AllowHostArgs {
        allow_host: vec![],
        added: vec!["a.example.org".into()],
        problems: vec!["AUTO_PACKAGE_SOURCE_HOSTS: `com` is a single label".into()],
    };
    let sink = Events::default();
    args.record(&sink);
    let got = sink.0.lock().unwrap();
    assert!(
        matches!(&got[..], [Event::HostAllowed { host, .. }, Event::Warning { .. }] if host == "a.example.org"),
        "{got:?}"
    );
}
