//! Checks of the clap definitions that the parse tests in `tests.rs` do not
//! reach: which flags are hidden from the help and that they still parse.

use clap::{CommandFactory, Parser};

#[test]
fn the_vestigial_flags_stay_parsable_and_leave_the_help() {
    let cmd = crate::Cli::command();
    let build = cmd.find_subcommand("build").unwrap();
    let backend = build
        .get_arguments()
        .find(|a| a.get_id() == "agent_backend")
        .unwrap();
    assert!(backend.is_hide_set());
    assert!(
        crate::Cli::try_parse_from([
            "auto-package",
            "build",
            "curl",
            "--project",
            "P",
            "--agent-backend",
            "cli"
        ])
        .is_ok()
    );
}

#[test]
fn the_nvidia_redist_shorthand_is_in_the_help_and_parses() {
    let cmd = crate::Cli::command();
    for name in ["build", "update"] {
        let sub = cmd.find_subcommand(name).unwrap();
        let redist = sub
            .get_arguments()
            .find(|a| a.get_id() == "nvidia_redist")
            .unwrap();
        assert!(!redist.is_hide_set(), "{name} --nvidia-redist is hidden");
    }
    assert!(
        crate::Cli::try_parse_from([
            "auto-package",
            "update",
            "--project",
            "P",
            "--nvidia-redist"
        ])
        .is_ok()
    );
}
