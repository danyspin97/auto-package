//! Tests for the tool allow-list of a turn and the bash command matching.

use super::*;

#[test]
fn refresh_turns_get_a_higher_turn_cap_and_the_others_keep_the_default() {
    assert_eq!(max_agent_turns(Task::RefreshPatches), 80);
    for task in [Task::Review, Task::FixAfterBuild, Task::PostBuildReview] {
        assert_eq!(max_agent_turns(task), MAX_AGENT_TURNS);
    }
    assert_eq!(MAX_AGENT_TURNS, 40);
}

#[test]
fn turn_tools_have_no_rpmspec_and_no_bare_quilt_and_deny_settings_writes() {
    let ws = std::path::Path::new("/srv/ws");
    let co = Some(std::path::Path::new("/srv/ws/checkout"));
    for checkout in [co, None] {
        let tools = turn_allowed_tools(Task::FixAfterBuild, ws, checkout);
        assert!(!tools.contains("rpmspec"), "{tools}");
        assert!(!tools.contains("quilt *"), "{tools}");
    }
    let denied = turn_denied_tools(ws, co);
    // `quilt diff` is granted, but not the option that runs a program.
    assert!(denied.contains("Bash(quilt *--diff=*)"), "{denied}");
    assert!(denied.contains("Write(//srv/ws/.claude/**)"));
    assert!(denied.contains("Edit(//srv/ws/checkout/.mcp.json)"));
}

#[test]
fn quilt_grant_is_denied_absolute_parent_and_chaining_forms() {
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, None);
    let list: Vec<&str> = denied.split(',').collect();
    for prefix in ["quilt", "cd source && quilt"] {
        for rule in [
            "* /*",
            "*..*",
            "*~*",
            "*$*",
            "*quiltrc*",
            "*;*",
            "*&*",
            "*|*",
            "*`*",
            "*>*",
            "*<*",
        ] {
            let rule = format!("Bash({prefix} {rule})");
            assert!(list.contains(&rule.as_str()), "missing {rule}: {denied}");
        }
    }
    // A relative path is not denied: the path rules need a space before
    // the `/`, and no rule is a bare `quilt */*`.
    assert!(!list.contains(&"Bash(quilt */*)"), "{denied}");
    assert!(!denied.contains("Bash(quilt *)"), "{denied}");
    // The grants themselves are still present for the checkout turns.
    let tools = turn_allowed_tools(
        Task::FixAfterBuild,
        ws,
        Some(std::path::Path::new("/srv/ws/checkout")),
    );
    assert!(tools.contains("Bash(quilt add *)"));
    assert!(tools.contains("Bash(cd source && quilt add *)"));
}

#[test]
fn turn_tools_cover_the_quilt_forms_the_prompts_use() {
    // Checked with `claude -p --allowed-tools "Bash(git init *)"`: a rule
    // `x *` also matches the bare `x`, and `cd <dir> && x` is split and
    // each part checked, so no separate bare-form rules are needed.
    let tools = turn_allowed_tools(
        Task::FixAfterBuild,
        std::path::Path::new("/srv/ws"),
        Some(std::path::Path::new("/srv/ws/checkout")),
    );
    let list: Vec<&str> = tools.split(',').collect();
    for sub in ["new", "add", "refresh", "series", "top", "applied"] {
        assert!(list.contains(&format!("Bash(quilt {sub} *)").as_str()));
        assert!(list.contains(&format!("Bash(cd source && quilt {sub} *)").as_str()));
    }
}

#[test]
fn the_refresh_turn_has_one_quilt_call_grant_and_no_env_prefix_or_mkdir() {
    // The single `cd source && quilt <sub>` call is what the prompt asks
    // for; chains, environment prefixes and scratch directories stay refused.
    let tools = turn_allowed_tools(
        Task::RefreshPatches,
        std::path::Path::new("/srv/ws"),
        Some(std::path::Path::new("/srv/ws/checkout")),
    );
    let list: Vec<&str> = tools.split(',').collect();
    assert!(list.contains(&"Bash(cd source && quilt pop *)"), "{tools}");
    assert!(
        list.contains(&"Bash(cd /srv/ws/source && quilt push *)"),
        "{tools}"
    );
    assert!(!tools.contains("mkdir"), "{tools}");
    assert!(!tools.contains("QUILT_"), "{tools}");
}

#[test]
fn skills_do_not_tell_the_turn_to_run_rpmspec() {
    let skills = concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging-skills");
    for entry in std::fs::read_dir(skills).unwrap() {
        let path = entry.unwrap().path().join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        assert!(
            !text.contains("rpmspec"),
            "{} mentions rpmspec, which the turn cannot run",
            path.display()
        );
    }
}

#[test]
fn build_turn_tools_exclude_rm_curl_web_search_and_mutating_zypper() {
    let tools = turn_allowed_tools(
        Task::FixAfterBuild,
        std::path::Path::new("/srv/ws"),
        Some(std::path::Path::new("/srv/ws/checkout")),
    );
    let list: Vec<&str> = tools.split(',').collect();
    for banned in ["Bash(rm *)", "Bash(zypper *)", "WebSearch", "Bash(curl"] {
        assert!(
            !tools.contains(banned),
            "{banned} must not be allowed: {tools}"
        );
    }
    // zypper is limited to the queries the conventions rely on.
    assert!(list.contains(&"Bash(zypper search *)"));
    assert!(list.contains(&"Bash(zypper what-provides *)"));
    assert!(list.contains(&"WebFetch(domain:pypi.org)"));
    assert!(
        !list.contains(&"Read"),
        "bare Read would reach the whole filesystem"
    );
    assert!(list.contains(&"Read(//srv/ws/**)"));
    assert!(list.contains(&"Read(//srv/ws/checkout/**)"));
}

#[test]
fn build_turn_without_checkout_reads_only_the_workspace() {
    let tools = turn_allowed_tools(Task::FixAfterBuild, std::path::Path::new("/srv/ws"), None);
    assert!(tools.split(',').any(|t| t == "Read(//srv/ws/**)"));
    assert!(!tools.contains("quilt"));
    assert_eq!(tools.matches("Read(").count(), 1);
}

#[test]
fn rpm_listing_is_granted_only_under_the_rpms_directory() {
    assert_eq!(rpm_inspection_tools(None), "");
    let tools = rpm_inspection_tools(Some(std::path::Path::new("/srv/nonexistent/rpms")));
    assert_eq!(tools, ",Bash(rpm -qlp /srv/nonexistent/rpms/*)");
    assert!(!tools.contains("rpm *") && !tools.contains("rpm -q *"));
}

#[test]
fn rpm_grant_is_denied_expansion_and_chaining_forms() {
    let ws = std::path::Path::new("/srv/ws");
    let denied = turn_denied_tools(ws, None);
    let list: Vec<&str> = denied.split(',').collect();
    // The grant's trailing `*` also matches these; the deny must win.
    for rule in [
        "Bash(rpm * --eval*)",
        "Bash(rpm * --define*)",
        "Bash(rpm * --macros*)",
        "Bash(rpm * --rcfile*)",
        "Bash(rpm * --pipe*)",
        "Bash(rpm * -*E*)",
        "Bash(rpm * -*D*)",
        "Bash(rpm *;*)",
        "Bash(rpm *&*)",
        "Bash(rpm *|*)",
        "Bash(rpm *`*)",
        "Bash(rpm *$(*)",
        "Bash(rpm *%(*)",
        "Bash(rpm *>*)",
        "Bash(rpm *<*)",
    ] {
        assert!(list.contains(&rule), "missing {rule}: {denied}");
    }
    // The grant itself is not denied, and a bare rpm never is granted.
    assert!(!denied.contains("Bash(rpm -qlp"), "{denied}");
    let granted = rpm_inspection_tools(Some(std::path::Path::new("/srv/nonexistent/rpms")));
    assert!(granted.contains("Bash(rpm -qlp /srv/nonexistent/rpms/*)"));
}

/// Matches a permission pattern (only `*` is special) against a whole command.
pub(super) fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The inner patterns of the `Bash(...)` deny rules in `denied`.
pub(super) fn bash_deny_patterns(denied: &str) -> Vec<&str> {
    denied
        .split(',')
        .filter_map(|r| r.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')))
        .collect()
}

/// The form the CLI matches a rule or a command in: a tab or a newline is
/// a space, so a rule `*<tab>*` is the rule `* *`.
fn cli_form(text: &str) -> String {
    text.replace(['\t', '\n'], " ")
}

/// Whether `command` is allowed by the `Bash(...)` grants of `tools` and no
/// deny of `denied` matches it. A grant `x *` also matches the bare `x`.
/// Rules and the command are compared in the CLI's form ([`cli_form`]).
pub(super) fn command_allowed(tools: &str, denied: &str, command: &str) -> bool {
    let command = cli_form(command);
    let granted = bash_deny_patterns(tools).iter().any(|p| {
        let p = cli_form(p);
        glob_matches(&p, &command) || p.strip_suffix(" *") == Some(command.as_str())
    });
    let refused = bash_deny_patterns(denied)
        .iter()
        .any(|p| glob_matches(&cli_form(p), &command));
    granted && !refused
}

#[test]
fn the_commands_a_refresh_turn_was_refused_are_allowed() {
    // Commands one refresh-patches turn ran that the sandbox refused, with
    // the workspace and checkout paths of that turn.
    let base = "/work/ap-work/home_jdoe_auto-package_mariadb-refresh2-x86_64";
    let ws = std::path::PathBuf::from(format!("{base}/state/mariadb"));
    let co = std::path::PathBuf::from(format!(
        "{base}/home:jdoe:auto-package:mariadb-refresh2/mariadb"
    ));
    let tools = turn_allowed_tools(Task::RefreshPatches, &ws, Some(&co));
    let denied = turn_denied_tools(&ws, Some(&co));
    let src = format!("cd {base}/state/mariadb/source && ");
    let in_workspace = format!("{src}quilt refresh -p ab --no-timestamps --no-index");
    for command in [
        in_workspace.as_str(),
        "quilt refresh -p ab --no-timestamps --no-index",
        "quilt refresh -p ab --no-timestamps --no-index mariadb-10.2.4-logrotate.patch",
        "quilt refresh --no-timestamps --no-index",
        "cd source && quilt refresh -p ab --no-timestamps --no-index",
        "cd source && quilt refresh -p 0 --no-timestamps --no-index",
        "cd source && quilt pop -a",
        "quilt pop -a",
        "cd source && quilt push",
        "cd source && quilt series",
        "cd source && quilt diff",
        "quilt top",
        "ls -la sdist",
    ] {
        assert!(
            command_allowed(&tools, &denied, command),
            "{command:?} is refused"
        );
    }
}

#[test]
fn dangerous_variants_of_the_granted_commands_stay_refused() {
    let base = "/work/ap-work/home_jdoe_auto-package_mariadb-refresh2-x86_64";
    let ws = std::path::PathBuf::from(format!("{base}/state/mariadb"));
    let co = std::path::PathBuf::from(format!(
        "{base}/home:jdoe:auto-package:mariadb-refresh2/mariadb"
    ));
    let rpms = std::path::PathBuf::from(format!("{base}/rpms"));
    let tools = format!(
        "{}{}",
        turn_allowed_tools(Task::RefreshPatches, &ws, Some(&co)),
        rpm_inspection_tools(Some(&rpms))
    );
    let denied = turn_denied_tools(&ws, Some(&co));
    let in_workspace = format!("cd {base}/state/mariadb/source && ");
    let inside = format!("{in_workspace}quilt refresh -p ab --no-index {base}/rpms/fix.patch");
    for command in [
        // Absolute paths, outside the tree or inside it, in every spelling.
        "quilt refresh -p ab --no-index /etc/passwd",
        "quilt refresh -p ab --no-index --opt=/etc/passwd",
        "cd source && quilt refresh -p ab --no-index \"/etc/passwd\"",
        "quilt add x\t/etc/hostname",
        "quilt add x\t\"/etc/hostname\"",
        "ls -la /etc",
        "ls\t-la\t/etc",
        "ls -d /usr/local/cuda-*",
        inside.as_str(),
        // Another patches directory, by option.
        "quilt push -P /etc",
        "cd source && quilt push -P ../../x",
        "quilt push -P ~/patches",
        // Parent paths, brace, glob, quote and substitution forms.
        "quilt refresh -p ab --no-index ../x",
        "quilt refresh -p ab --no-index {,/etc/x}",
        "quilt refresh -p ab --no-index $(id)",
        "quilt refresh -p ab --no-index `id`",
        "quilt refresh -p ab --no-index f?.patch",
        "ls -la ..",
        // `import` and `rename` are not granted: `import -P .o*/_apiurl` names
        // a patch by glob, and no prompt uses either subcommand.
        "quilt import -P .o*/_apiurl evil.txt",
        "cd source && quilt import -P .g*/config evil.txt",
        "quilt rename x.patch y.patch",
        // Chaining, redirection and pipes.
        "cd source && quilt pop -a; rm -rf x",
        "cd source && quilt pop -a && rm -rf x",
        "cd source && quilt pop -a 2>&1 | tail -3",
        "cd source && quilt refresh -p ab --no-index > /etc/x",
        "quilt refresh -p ab --no-index | tee x",
        // Commands that are not granted.
        "quilt setup mariadb.spec",
        "quilt edit fix.patch",
        "quilt shell",
        "cd /etc && quilt pop -a",
        "echo \"QP=$QUILT_PATCHES\"",
        "cat ~/.quiltrc",
        "readelf -d @/etc/x",
        "readelf @x",
    ] {
        assert!(
            !command_allowed(&tools, &denied, command),
            "{command:?} is allowed"
        );
    }
    // rpm listing of the rpms directory is granted; its expansion options
    // are not, even after the file name.
    let listing = format!("rpm -qlp {base}/rpms/a.rpm");
    assert!(command_allowed(&tools, &denied, &listing), "{listing:?}");
    let eval = format!("{listing} --eval '%(id)'");
    assert!(!command_allowed(&tools, &denied, &eval), "{eval:?}");
}

#[test]
fn denied_bash_commands_are_read_from_the_result_event() {
    let denials: Vec<serde_json::Value> = serde_json::from_str(
        r#"[{"tool_name":"Bash","tool_use_id":"x","tool_input":{"command":"patch -p1 x"}},
                {"tool_name":"Write","tool_use_id":"y","tool_input":{"file_path":"/x"}}]"#,
    )
    .unwrap();
    assert_eq!(denied_bash_commands(&denials), ["patch -p1 x"]);
    let ev: ResultEvent = serde_json::from_str(r#"{"result":"ok"}"#).unwrap();
    assert!(ev.permission_denials.is_empty());
}

#[test]
fn the_refresh_commands_are_allowed_and_dangerous_forms_are_not() {
    let ws = std::path::Path::new("/srv/ws");
    let co = std::path::Path::new("/srv/co/pkg");
    let tools = turn_allowed_tools(Task::RefreshPatches, ws, Some(co));
    let denied = turn_denied_tools(ws, Some(co));
    for command in [
        "cd source && quilt pop -a",
        "cd source && quilt push",
        "cd source && quilt push -f",
        "cd source && quilt push -a",
        "cd source && quilt push --fuzz=0 -v",
        "cd source && quilt pop -f",
        "cd source && quilt refresh -p ab --no-timestamps --no-index",
        "cd source && quilt refresh -p 0 --no-timestamps --no-index",
        "cd source && quilt delete -r fix.patch",
        "cd source && quilt diff",
        "quilt series",
        "quilt top",
        "quilt files",
        "quilt grep foo",
        "cd /srv/ws/source && quilt push -f",
        "cd source && patch -p1 --dry-run --fuzz=0 -f -i fix.patch",
        "cd source && patch -R -p1 --dry-run -i fix.patch",
        "patch -p0 --dry-run -i fix.patch",
        "patch --dry-run -p0 -i fix.patch",
        "cd source && patch --dry-run -p0 -i fix.patch",
        "cd /srv/ws/source && patch -p1 --dry-run -i fix.patch",
        "cd /srv/ws/source && patch --dry-run -p1 -i fix.patch",
    ] {
        assert!(
            command_allowed(&tools, &denied, command),
            "{command:?} is refused"
        );
    }
    for command in [
        "cd source && quilt edit f.c",
        "quilt setup x.spec",
        "quilt shell",
        "quilt mail",
        "quilt diff --diff=sh",
        "quilt diff --diff vim",
        "quilt header -e fix.patch",
        "quilt --quiltrc x push",
        "quilt add /etc/shadow",
        "cd source && quilt push; rm -rf x",
        "cd /srv/ws/source && quilt add ../../secret",
        "patch -p1 -i fix.patch",
        "cd source && patch -p1 < fix.patch",
        "patch -p1 --dry-run -i /etc/shadow",
        "patch -p1 --dry-run -i ../../x.patch",
        "patch -p1 --dry-run -o out -i fix.patch",
        "patch -p1 --dry-run -b -i fix.patch",
        "patch -p1 --dry-run --backup -i fix.patch",
        "patch -p1 --dry-run -e -i fix.patch",
        "patch -p1 -i fix.patch -- --dry-run",
        "patch -p1 --dry-run -i fix.patch; patch -p1 -i fix.patch",
        "cd /srv/ws/source && patch -p1 -i fix.patch",
    ] {
        assert!(
            !command_allowed(&tools, &denied, command),
            "{command:?} is allowed"
        );
    }
}

#[test]
fn the_dry_run_shapes_a_refresh_turn_sent_stay_refused_and_the_prompt_says_why() {
    // The two `patch --dry-run` checks one turn sent and the sandbox
    // refused. Both name the checkout's patch with `../../../`, the only
    // way out of `source/` to it: a parent path is refused by design.
    let base = "/work/ap-work/home_jdoe_auto-package_mariadb-refresh4-x86_64";
    let ws = std::path::PathBuf::from(format!("{base}/state/mariadb"));
    let co = std::path::PathBuf::from(format!(
        "{base}/home:jdoe:auto-package:mariadb-refresh4/mariadb"
    ));
    let tools = turn_allowed_tools(Task::RefreshPatches, &ws, Some(&co));
    let denied = turn_denied_tools(&ws, Some(&co));
    let patch =
        "../../../home:jdoe:auto-package:mariadb-refresh4/mariadb/mariadb-10.2.4-logrotate.patch";
    for command in [
        format!("cd {base}/state/mariadb/source && patch -p0 --dry-run -i {patch}"),
        format!("cd source && patch --dry-run -p0 -i {patch}"),
        format!("patch --dry-run -p0 -i {patch}"),
    ] {
        assert!(
            !command_allowed(&tools, &denied, &command),
            "{command:?} is allowed"
        );
    }
    // The forms that name a workspace-relative file stay granted.
    for command in [
        "patch --dry-run -p0 -i fix.patch",
        "patch -p0 --dry-run -i fix.patch",
        "cd source && patch -p0 --dry-run -i fix.patch",
    ] {
        assert!(
            command_allowed(&tools, &denied, command),
            "{command:?} is refused"
        );
    }
    // A checkout patch cannot be named without `..`, so the prompt has to
    // say that a dry run cannot read it and that quilt is the check.
    let prompt = refresh_patches_task();
    assert!(prompt.contains("outside the workspace"), "{prompt}");
    assert!(prompt.contains("check each patch with quilt"), "{prompt}");
    assert!(!prompt.contains("(relative paths only)"), "{prompt}");
}

#[test]
fn the_refresh_commands_work_in_the_turn_s_environment() {
    let have = |p: &str| {
        std::process::Command::new(p)
            .arg("--version")
            .env("QUILTRC", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !have("quilt") || !have("patch") {
        eprintln!("quilt or patch is not installed; skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    let co = root.path().join("co");
    std::fs::create_dir_all(ws.join("source")).unwrap();
    std::fs::create_dir_all(&co).unwrap();
    std::fs::write(ws.join("source/f.txt"), "a\nb2\nc\nd\n").unwrap();
    // Made against the old `b`: it does not apply to the new sources.
    let fix = "PATCH-FIX-UPSTREAM fix.patch gh#1\n---\n--- a/f.txt\n+++ b/f.txt\n\
                   @@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n";
    std::fs::write(co.join("fix.patch"), fix).unwrap();
    std::fs::write(co.join("series"), "fix.patch\n").unwrap();
    std::fs::write(ws.join("source/fix.patch"), fix).unwrap();
    let tools = turn_allowed_tools(Task::RefreshPatches, &ws, Some(&co));
    let denied = turn_denied_tools(&ws, Some(&co));
    let run = |command: &str| {
        assert!(
            command_allowed(&tools, &denied, command),
            "{command:?} is refused"
        );
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ws)
            .env_clear()
            .envs(crate::proc::minimal_env(&[]))
            .env("LC_ALL", "C")
            .envs(quilt_env(Some(&co)))
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, out) = run("cd source && quilt push");
    assert!(!ok && out.contains("does not apply"), "{out}");
    let (ok, out) = run("cd source && patch -p1 --dry-run --fuzz=0 -f -i fix.patch");
    assert!(!ok && out.contains("FAILED"), "{out}");
    let (_, out) = run("cd source && quilt push -f");
    assert!(out.contains("forced"), "{out}");
    // The agent ports the hunk by editing the file.
    std::fs::write(ws.join("source/f.txt"), "a\nB\nc\nd\n").unwrap();
    let (ok, out) = run("cd source && quilt refresh -p ab --no-timestamps --no-index");
    assert!(ok, "{out}");
    let refreshed = std::fs::read_to_string(co.join("fix.patch")).unwrap();
    assert!(
        refreshed.starts_with("PATCH-FIX-UPSTREAM fix.patch gh#1"),
        "{refreshed}"
    );
    assert!(refreshed.contains("-b2\n+B"), "{refreshed}");
    let (ok, out) = run("cd source && quilt pop -a");
    assert!(ok, "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("source/f.txt")).unwrap(),
        "a\nb2\nc\nd\n"
    );
    let (ok, out) = run("cd source && quilt push");
    assert!(ok, "the refreshed patch applies: {out}");
}
