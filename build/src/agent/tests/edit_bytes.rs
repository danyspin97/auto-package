//! Tests for the byte-edit helper's grant: where it is granted, the shapes it
//! may run, and the shapes the denies refuse.

use super::tools::*;
use super::*;

const CMD: &str = "/opt/ap/bin/auto-package";

#[test]
fn only_a_refresh_turn_is_granted_the_helper() {
    assert_eq!(
        edit_bytes_grant(Task::RefreshPatches, Some(CMD)),
        ",Bash(/opt/ap/bin/auto-package edit-bytes *)"
    );
    for task in [Task::FixAfterBuild, Task::Review, Task::PatchDependency] {
        assert_eq!(edit_bytes_grant(task, Some(CMD)), "", "{task:?}");
    }
    // No spellable path, no grant at all.
    assert_eq!(edit_bytes_grant(Task::RefreshPatches, None), "");
}

#[test]
fn a_path_a_rule_cannot_spell_gets_no_command() {
    assert_eq!(
        spellable_command("/usr/bin/auto-package").as_deref(),
        Some("/usr/bin/auto-package")
    );
    for path in [
        "relative/auto-package",
        "/home/a b/auto-package",
        "/x/(deleted)",
        "/x/*/auto-package",
        "/x/auto-package\"",
    ] {
        assert_eq!(spellable_command(path), None, "{path}");
    }
}

#[test]
fn the_turn_grants_and_denies_name_the_running_binary() {
    let ws = std::path::Path::new("/srv/ws");
    let co = std::path::Path::new("/srv/ws/checkout");
    let Some(cmd) = edit_bytes_command() else {
        return;
    };
    let tools = turn_allowed_tools(Task::RefreshPatches, ws, Some(co));
    assert!(
        tools.contains(&format!("Bash({cmd} edit-bytes *)")),
        "{tools}"
    );
    let denied = turn_denied_tools(ws, Some(co));
    assert!(denied.contains(&format!("Bash({cmd} *\"*)")), "{denied}");
    let other = turn_allowed_tools(Task::FixAfterBuild, ws, Some(co));
    assert!(!other.contains("edit-bytes"), "{other}");
}

/// The grant and the denies of the helper, as a turn would have them.
fn grant_and_denies() -> (String, String) {
    let ws = std::path::Path::new("/srv/ws");
    let tools = edit_bytes_grant(Task::RefreshPatches, Some(CMD));
    let denied = edit_bytes_denies(ws, Some(CMD)).join(",");
    (tools, denied)
}

fn helper(args: &str) -> String {
    format!("{CMD} edit-bytes {args}")
}

#[test]
fn the_documented_shapes_are_allowed() {
    let (tools, denied) = grant_and_denies();
    for command in [
        helper(
            "source/main/mysqldump.result --old-file byte-edit/old.txt --new-file byte-edit/new.txt",
        ),
        helper("source/a.txt --old-file byte-edit/o --new-file byte-edit/n"),
        helper("source/lib/x-1.0/y.c --old-file byte-edit/old.txt --new-file byte-edit/new.txt"),
    ] {
        assert!(command_allowed(&tools, &denied, &command), "{command}");
    }
}

#[test]
fn absolute_parent_and_outside_paths_are_refused() {
    let (tools, denied) = grant_and_denies();
    let good = "--old-file byte-edit/old.txt --new-file byte-edit/new.txt";
    for args in [
        format!("/etc/passwd {good}"),
        format!("source/x --old-file /etc/passwd --new-file byte-edit/new.txt"),
        format!("source/x --old-file=/etc/passwd --new-file byte-edit/new.txt"),
        format!("source/x --old-file byte-edit/o --new-file=/abs"),
        format!("source/../notes.md {good}"),
        format!("source/x --old-file ../outside --new-file byte-edit/new.txt"),
        format!("source/x --old-file byte-edit/o --new-file ~/n"),
    ] {
        let command = helper(&args);
        assert!(!command_allowed(&tools, &denied, &command), "{command}");
    }
}

#[test]
fn chaining_substitution_redirection_and_quotes_are_refused() {
    let (tools, denied) = grant_and_denies();
    let good = "source/x --old-file byte-edit/o --new-file byte-edit/n";
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
    ] {
        let command = format!("{}{suffix}", helper(good));
        assert!(!command_allowed(&tools, &denied, &command), "{command}");
    }
    for args in [
        "\"source/x\" --old-file byte-edit/o --new-file byte-edit/n",
        "'source/x' --old-file byte-edit/o --new-file byte-edit/n",
        "source/x --old-file \"byte-edit/o\" --new-file byte-edit/n",
        "source/[ab].c --old-file byte-edit/o --new-file byte-edit/n",
        "source/{a,b}.c --old-file byte-edit/o --new-file byte-edit/n",
    ] {
        let command = helper(args);
        assert!(!command_allowed(&tools, &denied, &command), "{command}");
    }
}
