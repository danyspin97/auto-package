//! Tests for the package-lookup helper's grant: which turns have it, the
//! shapes it may run, and the shapes the denies refuse.

use super::tools::*;
use super::*;

const CMD: &str = "/opt/ap/bin/auto-package";

fn grant_and_denies() -> (String, String) {
    let ws = std::path::Path::new("/srv/ws");
    let tools = provides_grant(Task::FixAfterBuild, Some(CMD));
    let denied = edit_bytes_denies(ws, Some(CMD)).join(",");
    (tools, denied)
}

#[test]
fn editing_turns_are_granted_the_lookup_and_read_only_ones_are_not() {
    assert_eq!(
        provides_grant(Task::FixAfterBuild, Some(CMD)),
        ",Bash(/opt/ap/bin/auto-package provides *)"
    );
    assert!(provides_grant(Task::Review, Some(CMD)).contains("provides"));
    for task in [Task::TranslateUnresolvableDeps, Task::RequestReview] {
        assert_eq!(provides_grant(task, Some(CMD)), "", "{task:?}");
    }
    assert_eq!(provides_grant(Task::FixAfterBuild, None), "");
}

#[test]
fn the_turn_grants_name_the_running_binary() {
    let ws = std::path::Path::new("/srv/ws");
    let co = std::path::Path::new("/srv/ws/checkout");
    let Some(cmd) = edit_bytes_command() else {
        return;
    };
    for task in [Task::FixAfterBuild, Task::Review] {
        let tools = turn_allowed_tools(task, ws, Some(co));
        assert!(
            tools.contains(&format!("Bash({cmd} provides *)")),
            "{tools}"
        );
    }
}

#[test]
fn the_documented_calls_are_allowed() {
    let (tools, denied) = grant_and_denies();
    for args in [
        "pkgconfig xkbcommon",
        "lib pam",
        "lib -lpam",
        "cmake Qt5Core",
        "file usr/include/foo.h",
        "file usr/bin/pg_config",
    ] {
        let command = format!("{CMD} provides {args}");
        assert!(command_allowed(&tools, &denied, &command), "{command}");
    }
}

#[test]
fn chaining_substitution_redirection_and_quotes_are_refused() {
    let (tools, denied) = grant_and_denies();
    for suffix in [
        "; rm -rf source",
        " && cat /etc/passwd",
        " | sh",
        " & id",
        " $(id)",
        " `id`",
        " > /tmp/out",
        " < /etc/passwd",
        " || true",
        " 'a'",
        " \"a\"",
    ] {
        let command = format!("{CMD} provides lib pam{suffix}");
        assert!(!command_allowed(&tools, &denied, &command), "{command}");
    }
}

#[test]
fn no_other_subcommand_is_granted() {
    let (tools, denied) = grant_and_denies();
    let command = format!("{CMD} provides file /etc/passwd");
    assert!(!command_allowed(&tools, &denied, &command), "{command}");
    for sub in ["edit-bytes x", "build x", "update x", "doctor"] {
        let command = format!("{CMD} {sub}");
        assert!(!command_allowed(&tools, &denied, &command), "{command}");
    }
}

#[test]
fn the_text_names_each_call_and_the_refused_zypper_form() {
    let text = provides_text(Task::FixAfterBuild, Some(CMD));
    for needle in [
        "auto-package provides pkgconfig <module>",
        "auto-package provides lib <name>",
        "auto-package provides cmake <Package>",
        "auto-package provides file usr/bin/<tool>",
        "do not try other spellings",
        "reliable only for a tool or file in package metadata",
        "can differ from the project's targets",
        "`unverified`",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(!text.contains("usr/include"), "{text}");
    assert_eq!(provides_text(Task::FixAfterBuild, None), "");
    assert_eq!(provides_text(Task::RequestReview, Some(CMD)), "");
}

#[test]
fn the_lookup_is_granted_only_to_turns_that_have_it() {
    for task in [Task::TranslateUnresolvableDeps, Task::RequestReview] {
        assert!(!lookup_granted(task), "{task:?}");
    }
    assert_eq!(
        lookup_granted(Task::FixAfterBuild),
        edit_bytes_command().is_some()
    );
}
