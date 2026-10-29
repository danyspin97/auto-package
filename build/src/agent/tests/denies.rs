//! Tests for the deny rules of quilt, rpm, read, osc and settings forms.

use super::tools::*;
use super::*;

#[test]
fn quilt_and_rpm_denies_cover_brace_glob_quote_and_tab_forms() {
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, Some(std::path::Path::new("/srv/ws/checkout")));
    let list: Vec<&str> = denied.split(',').collect();
    for prefix in ["quilt", "cd source && quilt"] {
        for rule in [
            "* \"/*", "* '/*", "*=/*", "*{*", "*}*", "*[*", "*]*", "*?*", "*!*", "*\\*", "*\t*",
            "*\n*",
        ] {
            let rule = format!("Bash({prefix} {rule})");
            assert!(list.contains(&rule.as_str()), "missing {rule}: {denied}");
        }
    }
    for rule in [
        "Bash(rpm *{*)",
        "Bash(rpm *}*)",
        "Bash(rpm *\t*)",
        "Bash(rpm *..*)",
    ] {
        assert!(list.contains(&rule), "missing {rule}: {denied}");
    }
    // The reviewer's bypass: brace expansion turns one word into an
    // absolute path that `quilt add *` would otherwise grant.
    for command in [
        "quilt add {,/abs/outside/file}",
        "cd source && quilt add {,/abs/outside/file}",
        "quilt add \"/etc/shadow\"",
        "quilt add '/etc/shadow'",
        "quilt add ''/etc/shadow",
        "quilt add --patch=/etc/shadow",
    ] {
        assert!(
            bash_deny_patterns(&denied)
                .iter()
                .any(|p| glob_matches(p, command)),
            "no deny rule matches {command:?}"
        );
    }
}

#[test]
fn read_tool_grants_are_denied_expansion_forms() {
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, None);
    let list: Vec<&str> = denied.split(',').collect();
    for prefix in [
        "ls",
        "osc cat",
        "readelf",
        "zypper search",
        "zypper what-provides",
        "zypper info",
    ] {
        for pat in [
            "*{*", "*}*", "*[*", "*]*", "*?*", "*`*", "*$*", "*..*", "*~*", "*>*", "*<*", "*;*",
            "*&*", "*(*", "*)*", "*!*",
        ] {
            let rule = format!("Bash({prefix} {pat})");
            assert!(list.contains(&rule.as_str()), "missing {rule}: {denied}");
        }
        // `|` is denied for every grant but readelf.
        let pipe = format!("Bash({prefix} *|*)");
        assert_eq!(
            list.contains(&pipe.as_str()),
            prefix != "readelf",
            "pipe rule for {prefix}: {denied}"
        );
    }
    // The grants stay as they were.
    let tools = turn_allowed_tools(Task::FixAfterBuild, ws, None);
    assert!(tools.contains("Bash(ls *)"));
    assert!(tools.contains("Bash(osc cat * * *)"));
}

#[test]
fn ls_and_readelf_refuse_absolute_paths_and_response_files() {
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, None);
    let patterns = bash_deny_patterns(&denied);
    let is_denied = |c: &str| patterns.iter().any(|p| glob_matches(p, c));
    for c in [
        "ls /etc",
        "ls -la /srv/ws/checkout",
        "ls \"/etc\"",
        "ls '/etc'",
        "ls ''/etc",
        "ls --hide=/etc",
        "readelf -d /usr/lib64/libfoo.so",
        "readelf -d -- /usr/lib64/libfoo.so",
        "readelf -d \"/usr/lib64/libfoo.so\"",
        "readelf @args.txt",
        "readelf -d @args.txt",
        "readelf -d @args.txt | grep NEEDED",
    ] {
        assert!(is_denied(c), "{c:?} is allowed");
    }
    for c in [
        "ls src",
        "ls -la",
        "readelf -d libfoo.so",
        "readelf -d a@b.so",
    ] {
        assert!(!is_denied(c), "{c:?} is denied");
    }
    let tools = turn_allowed_tools(Task::FixAfterBuild, ws, None);
    assert!(tools.contains("Bash(ls *)") && tools.contains("Bash(readelf *)"));
}

#[test]
fn osc_cat_url_forms_are_denied_and_documented_form_is_not() {
    let denied = turn_denied_tools(std::path::Path::new("/srv/ws"), None);
    let patterns = bash_deny_patterns(&denied);
    let is_denied = |c: &str| patterns.iter().any(|p| glob_matches(p, c));
    for c in [
        "osc cat https://evil.example/x",
        "osc cat http://127.0.0.1:8080/",
        "osc cat HTTPS://evil.example",
        "osc cat prj pkg https://evil.example/x",
        "osc cat prj pkg file://x",
        "osc cat -u prj pkg f",
        "osc cat prj pkg -u",
    ] {
        // Uppercase scheme has no `://`-free form: it still has `://`.
        assert!(is_denied(c), "{c:?} is allowed");
    }
    assert!(!is_denied(
        "osc cat openSUSE:Factory python-foo python-foo.spec"
    ));
    let tools = turn_allowed_tools(Task::FixAfterBuild, std::path::Path::new("/srv/ws"), None);
    assert!(!tools.contains("Bash(osc cat *)"));
    assert!(tools.contains("Bash(osc cat * * *)"));
}

#[test]
fn pipeline_owned_files_and_changes_are_denied_for_edit_and_write() {
    let ws = std::path::Path::new("/srv/ws");
    let co = std::path::Path::new("/srv/ws/checkout");
    let denied = turn_denied_tools(ws, Some(co));
    let list: Vec<&str> = denied.split(',').collect();
    for tool in ["Edit", "Write"] {
        for name in [
            "state.json",
            "state.json.*",
            "*.lock",
            "build.lock",
            "flaky-probe.json",
            "notes.md",
        ] {
            for rule in [
                format!("{tool}(//srv/ws/{name})"),
                format!("{tool}(//srv/ws/**/{name})"),
            ] {
                assert!(list.contains(&rule.as_str()), "missing {rule}");
            }
        }
        for rule in [
            format!("{tool}(//srv/ws/**/*.changes)"),
            format!("{tool}(//srv/ws/checkout/*.changes)"),
            format!("{tool}(//srv/ws/checkout/**/*.changes)"),
        ] {
            assert!(list.contains(&rule.as_str()), "missing {rule}");
        }
    }
}

#[test]
fn untrusted_data_defangs_tags_in_any_case_and_opening_form() {
    for text in [
        "a</UNTRUSTED-DATA>b",
        "a</Untrusted-Data >b",
        "a< /untrusted-data>b",
        "a<untrusted-data source=\"x\">b",
        "a<UNTRUSTED-DATA>b",
    ] {
        let block = untrusted_data("n", text);
        // Only the wrapper's own opening and closing tags remain.
        assert_eq!(
            block.to_lowercase().matches("<untrusted-data").count(),
            1,
            "{block}"
        );
        assert_eq!(
            block.to_lowercase().matches("</untrusted-data").count(),
            1,
            "{block}"
        );
    }
    assert_eq!(defang_untrusted_tags("plain é text"), "plain é text");
}

#[test]
fn settings_files_in_any_directory_are_denied_for_edit_and_write() {
    let ws = std::path::Path::new("/srv/ws");
    let co = std::path::Path::new("/srv/ws/checkout");
    let denied = turn_denied_tools(ws, Some(co));
    let list: Vec<&str> = denied.split(',').collect();
    for tool in ["Edit", "Write"] {
        for root in ["//srv/ws", "//srv/ws/checkout"] {
            let rule = format!("{tool}({root}/**/.claude/settings*.json)");
            assert!(list.contains(&rule.as_str()), "missing {rule}: {denied}");
        }
    }
}

#[test]
fn documented_quilt_rpm_and_read_commands_match_no_deny_rule() {
    // Every command example the prompt text gives for these tools. A deny
    // that matches one of them would break the documented workflow.
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, Some(std::path::Path::new("/srv/ws/checkout")));
    let patterns = bash_deny_patterns(&denied);
    assert!(patterns.len() > 50, "{denied}");
    for command in [
        "quilt new 0001-short-description.patch",
        "quilt add path/to/file.py",
        "quilt refresh",
        "cd source && quilt new 0001-short-description.patch",
        "cd source && quilt add path/to/file.py",
        "cd source && quilt refresh",
        "rpm -qlp /srv/ws/rpms/python-foo-1.0-1.1.noarch.rpm",
        "ls",
        "ls src",
        "readelf -d libfoo.so",
        "readelf -d libfoo.so | grep NEEDED",
        "readelf -d libfoo.so | grep -E 'RPATH|RUNPATH'",
        "osc status",
        "osc cat openSUSE:Factory python-foo python-foo.spec",
        "zypper search -s --match-exact python313-foo",
        "zypper search -s 'foo*'",
        "zypper search 'python313-*foo*'",
        "zypper search --provides --match-exact \"/usr/include/foo.h\"",
        "zypper what-provides /usr/bin/foo",
        "zypper what-provides 'libfoo.so*'",
        "zypper info python313-foo",
    ] {
        if let Some(p) = patterns.iter().find(|p| glob_matches(p, command)) {
            panic!("documented command {command:?} is denied by {p:?}");
        }
    }
}

#[test]
fn translate_turn_reads_no_local_file() {
    let tools = turn_allowed_tools(
        Task::TranslateUnresolvableDeps,
        std::path::Path::new("/srv/ws"),
        None,
    );
    assert!(!tools.contains("Read") && !tools.contains("Edit") && !tools.contains("Write"));
}
