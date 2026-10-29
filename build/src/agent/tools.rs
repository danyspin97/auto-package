//! The tools a turn may use and the ones it may not: the allow-list per task,
//! the deny rules for quilt, rpm, read and settings forms, and the pipeline-owned
//! files.

use super::*;

/// The turn tools when a checkout exists, before the quilt and `patch` grants
/// of [`patch_tools`]. `rpmspec` is absent, because `%(...)` in a spec runs
/// shell commands; the pipeline runs it itself on the turn's result.
const CHECKOUT_TOOLS: &str = concat!(
    "Grep,Skill,Bash(osc status),Bash(ls *),Bash(zypper search *),",
    "Bash(zypper what-provides *),Bash(zypper info *),Bash(osc cat * * *),Bash(readelf *),",
    "WebFetch(domain:pypi.org)"
);

/// The quilt subcommands a turn may run to make and rebase patches. Not
/// granted: `edit` (starts `$EDITOR`), `setup` (runs the spec's `%prep`),
/// `mail`, `shell` and `init`; the options that run a program or source a file
/// (`--diff=`, `--quiltrc`, `header -e`) are denied in [`quilt_denied_tools`].
const QUILT_SUBCOMMANDS: [&str; 23] = [
    "push",
    "pop",
    "refresh",
    "new",
    "add",
    "remove",
    "rename",
    "delete",
    "import",
    "header",
    "diff",
    "patches",
    "next",
    "previous",
    "fold",
    "revert",
    "snapshot",
    "series",
    "top",
    "applied",
    "unapplied",
    "files",
    "grep",
];

/// The prefixes a quilt or `patch` command may carry: bare (in `source/`),
/// `cd source && `, and `cd <workspace>/source && ` with the workspace path as
/// given and canonical (the turn has written both forms).
fn source_prefixes(workspace: &std::path::Path) -> Vec<String> {
    let mut out = vec![String::new(), "cd source && ".to_string()];
    let canonical = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    for ws in [workspace.to_path_buf(), canonical] {
        let prefix = format!("cd {}/source && ", ws.display());
        if !out.contains(&prefix) {
            out.push(prefix);
        }
    }
    out
}

/// The quilt and `patch` grants of a checkout turn, for every prefix of
/// [`source_prefixes`]. quilt is the tool for patches; `patch` is granted only
/// with `--dry-run`, as a read-only check (the denies of [`patch_denied_tools`]
/// keep it from writing or reading outside the tree).
fn patch_tools(workspace: &std::path::Path) -> String {
    let mut out = Vec::new();
    for prefix in source_prefixes(workspace) {
        for sub in QUILT_SUBCOMMANDS {
            out.push(format!("Bash({prefix}quilt {sub} *)"));
        }
        out.push(format!("Bash({prefix}patch *--dry-run*)"));
    }
    out.join(",")
}

/// The variables that make quilt work in the turn with no setup: the patches
/// are the checkout's files (`QUILT_PATCHES`), quilt's state is `.pc` in the
/// directory it runs in (`source/`), and no quiltrc is read: one is sourced as
/// shell, and without `QUILTRC` (there is no QUILT_QUILTRC) `~/.quiltrc` is.
pub(crate) fn quilt_env(
    checkout: Option<&std::path::Path>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    let mut env = vec![
        ("QUILT_PC", std::ffi::OsString::from(".pc")),
        ("QUILTRC", std::ffi::OsString::from("/dev/null")),
    ];
    if let Some(checkout) = checkout {
        env.push(("QUILT_PATCHES", checkout.as_os_str().to_owned()));
    }
    env
}

/// The quilt series a [`Task::RefreshPatches`] turn reads, a file the pipeline
/// writes into the checkout for that round (the spec's patches in rpm's order
/// and levels). It is never the checkout's own `series`.
pub(crate) const REFRESH_SERIES: &str = "series-rebase";

/// Tool allowlist for one agent turn.
///
/// The turn reads hostile upstream content (sdists, build logs, notes), so it
/// gets only what a spec edit needs. Edit, Write and Read are granted under
/// the directories the turn works in, not anywhere on disk (path rules are
/// written as `//<absolute path>`); there is no `rm`, only the read-only
/// `zypper search` and `what-provides` queries, and web
/// access is `WebFetch` on PyPI. The read-only translate task is the one
/// turn that may also search the web: it can touch no local file.
pub(super) fn turn_allowed_tools(
    task: Task,
    workspace: &std::path::Path,
    checkout: Option<&std::path::Path>,
) -> String {
    let patch_tools: String;
    let (base_tools, roots): (&str, Vec<&std::path::Path>) = if task
        == Task::TranslateUnresolvableDeps
    {
        // Read-only by construction, not just by prompt instruction: this
        // task only reports upstream names for `unresolvable_packages`, it
        // must not be able to touch the spec or checkout either way.
        ("WebFetch(domain:pypi.org),WebSearch", Vec::new())
    } else if let Some(checkout) = checkout {
        if task == Task::PatchDependency {
            // Reads the upstream history: the code to restore is not in the
            // release archive the checkout holds.
            patch_tools = format!(
                "{CHECKOUT_TOOLS},{},WebFetch(domain:github.com),\
                 WebFetch(domain:raw.githubusercontent.com),WebFetch(domain:api.github.com)",
                self::patch_tools(workspace)
            );
            (patch_tools.as_str(), vec![workspace, checkout])
        } else {
            patch_tools = format!("{CHECKOUT_TOOLS},{}", self::patch_tools(workspace));
            (patch_tools.as_str(), vec![workspace, checkout])
        }
    } else {
        (
            "Grep,Skill,Bash(osc status),Bash(ls *),Bash(zypper search *),Bash(zypper what-provides *),Bash(zypper info *),Bash(osc cat * * *),Bash(readelf *),WebFetch(domain:pypi.org)",
            vec![workspace],
        )
    };
    let mut tools = base_tools.to_string();
    for root in roots {
        let p = permission_path(root);
        tools.push_str(&format!(",Edit({p}/**),Write({p}/**),Read({p}/**)"));
    }
    tools
}

/// Read-only access to the checkouts of the packages this one depends on, so a
/// turn can read a sibling's spec and files before it reports a defect in it.
/// Reading only: a change to a sibling is requested with `patch_dependencies`.
pub(super) fn sibling_read_tools(dirs: &[std::path::PathBuf]) -> String {
    dirs.iter()
        .map(|d| {
            let d = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
            format!(",Read({}/**)", permission_path(&d))
        })
        .collect()
}

/// Read-only queries on the RPMs this project built, so a turn can tell what a
/// dependency really ships (convention 61) instead of guessing from an import
/// error. Tied to the `rpms/` directory by prefix: `rpm -q` takes `--eval`
/// and `--define`, so it is never granted as a bare command. Empty without a
/// directory.
pub(super) fn rpm_inspection_tools(rpms_dir: Option<&std::path::Path>) -> String {
    let Some(dir) = rpms_dir else {
        return String::new();
    };
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    format!(",Bash(rpm -qlp {}/*)", dir.display())
}

/// Files in the workspace that only the pipeline writes. The turn must not
/// edit them: `state.json` and the probe result steer later decisions, the
/// locks guard concurrent runs, and `notes.md` is appended by the pipeline.
const PIPELINE_OWNED_FILES: [&str; 8] = [
    "state.json",
    "state.json.*",
    "*.lock",
    "build.lock",
    "flaky-probe.json",
    "flaky-probe.json.*",
    "notes.md",
    "notes.md.*",
];

/// Project settings are read from the turn's directories, so a file the turn
/// wrote there (hooks, MCP servers, wider permissions) would take effect on the
/// next turn. Deny writing them.
pub(super) fn turn_denied_tools(
    workspace: &std::path::Path,
    checkout: Option<&std::path::Path>,
) -> String {
    let mut denied = Vec::new();
    for root in std::iter::once(workspace).chain(checkout) {
        let p = permission_path(root);
        for tool in ["Edit", "Write"] {
            denied.push(format!("{tool}({p}/.claude/**)"));
            denied.push(format!("{tool}({p}/.mcp.json)"));
            denied.push(format!("{tool}({p}/.claude.json)"));
            // Project settings in any subdirectory (`settings.json` and
            // `settings.local.json`), not only the root `.claude/`.
            denied.push(format!("{tool}({p}/**/.claude/settings*.json)"));
        }
    }
    // The convention files are the pipeline's, not the turn's to rewrite.
    let p = permission_path(workspace);
    for tool in ["Edit", "Write"] {
        denied.push(format!("{tool}({p}/{CONVENTIONS_DIR}/**)"));
        // The pipeline's own files live in the workspace the turn may write:
        // its state, locks, probe results and the notes it appends itself.
        for name in PIPELINE_OWNED_FILES {
            denied.push(format!("{tool}({p}/{name})"));
            denied.push(format!("{tool}({p}/**/{name})"));
        }
    }
    // A `.changes` entry comes from the typed `changelog` field, written by the
    // pipeline; its text reaches the commit message and the request description.
    for root in std::iter::once(workspace).chain(checkout) {
        let p = permission_path(root);
        for tool in ["Edit", "Write"] {
            denied.push(format!("{tool}({p}/**/*.changes)"));
            denied.push(format!("{tool}({p}/*.changes)"));
        }
    }
    denied.extend(rpm_denied_tools());
    denied.extend(quilt_denied_tools(workspace));
    denied.extend(patch_denied_tools(workspace));
    denied.extend(read_tool_denied_tools());
    denied.join(",")
}

/// Denies for the read-only Bash grants that take a path or a name and print
/// the result back to the model (`ls`, `osc cat`, `readelf`, `zypper`). Their
/// trailing `*` also matches brace or glob expansion, backticks, `$(`, `..`
/// and `~`, which can name files outside what the turn should see. Only these
/// metacharacters are denied: `ls -d lib/cuda-*` and the quoted
/// `zypper search 'python313-*<stem>*'` form stay allowed, and `*` is not
/// denied for that reason.
///
/// The chaining and redirection characters (`> < ; & ( ) !`) are denied too:
/// `ls <workspace> > ~/.bashrc` would write outside the turn's write set, and
/// the engine's handling of redirection targets is not something to rely on.
/// `|` is denied for every grant except `readelf`, because the prompt documents
/// `readelf -d <lib> | grep NEEDED` (`grep` has no grant of its own).
///
/// No tab or newline pattern: the CLI reads the whitespace of a rule as a
/// separator, so `*<tab>*` becomes `* *` and denies every command with two
/// words (see [`path_and_metachar_denies`]).
fn read_tool_denied_tools() -> Vec<String> {
    let prefixes = [
        "ls",
        "osc cat",
        "readelf",
        "zypper search",
        "zypper what-provides",
        "zypper info",
    ];
    let patterns = [
        "*{*", "*}*", "*[*", "*]*", "*?*", "*`*", "*$*", "*..*", "*~*", "*\\*", "*>*", "*<*",
        "*;*", "*&*", "*(*", "*)*", "*!*",
    ];
    let mut denied: Vec<String> = prefixes
        .iter()
        .flat_map(|p| patterns.iter().map(move |pat| format!("Bash({p} {pat})")))
        .collect();
    denied.extend(
        prefixes
            .iter()
            .filter(|p| **p != "readelf")
            .map(|p| format!("Bash({p} *|*)")),
    );
    // Absolute paths for `ls` and `readelf`, in any spelling the shell turns
    // into a path (plain, quoted, `--opt=/abs`, a quote pair hiding the `/`).
    // The grants are relative to the turn's directory, and a deny cannot
    // carve out the workspace, so an absolute path is refused even inside it:
    // the prompts name files relative to the checkout.
    for p in ["ls", "readelf"] {
        for pat in [
            "/*", "* /*", "\"/*", "* \"/*", "'/*", "* '/*", "*=/*", "\"\"*", "''*", "* \"\"*",
            "* ''*",
        ] {
            denied.push(format!("Bash({p} {pat})"));
        }
    }
    // `readelf @file` reads its arguments from a file (binutils expands a
    // leading `@`), so the response-file form is refused wherever it appears.
    denied.push("Bash(readelf @*)".into());
    denied.push("Bash(readelf * @*)".into());
    // `osc cat <url>` is an API GET to any host: no URL, and no option that
    // could change the API server.
    denied.push("Bash(osc cat *://*)".into());
    denied.push("Bash(osc cat http*)".into());
    denied.push("Bash(osc cat -*)".into());
    denied.push("Bash(osc cat * -*)".into());
    denied
}

/// Denies that win over the quilt grants in [`CHECKOUT_TOOLS`]. A grant's
/// trailing `*` matches any text, so `quilt add /abs/secret` or
/// `quilt add ../../x` would otherwise be allowed, and a following
/// `quilt refresh` would write that file's content into a patch the pipeline
/// commits. The path rules need a space before the `/` or `~`, so a relative
/// `quilt add foo/bar.c` is not denied. The metacharacter rules block chaining,
/// substitution and redirection inside a granted quilt command. Brace expansion
/// (`quilt add {,/abs/file}`) turns one word into several, and the grant's `*`
/// matches the result, so `{`/`}` are denied, as are glob characters that can
/// reach outside the checkout (`[`, `]`, `?`). Quoted absolute paths
/// (`"/abs"`, `'/abs'`, `--opt=/abs`) and a quote-pair prefix that hides the
/// leading `/` (`""/abs`) are denied too. Each rule is written for every
/// prefix of [`source_prefixes`], since the grant matches them as separate
/// prefixes. `--diff=UTILITY` runs a program and `header -e` an editor.
fn quilt_denied_tools(workspace: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = source_prefixes(workspace)
        .iter()
        .flat_map(|p| {
            ["*--diff=*", "*--diff *", "header *-e*", "header *--edit*"]
                .iter()
                .map(move |pat| format!("Bash({p}quilt {pat})"))
        })
        .collect();
    out.extend(path_and_metachar_denies(workspace, "quilt"));
    out
}

/// Denies that win over the `patch *--dry-run*` grant: no absolute or
/// escaping path, no shell metacharacter (as for quilt), and none of the
/// options that write a file even next to `--dry-run` or run a program: `-o`,
/// the backup options, the reject file, an `ed` script (`-e` pipes it to
/// `ed`), and `--` (after it `--dry-run` would be a file name).
fn patch_denied_tools(workspace: &std::path::Path) -> Vec<String> {
    let options = [
        "* -o*",
        "*--output*",
        "* -b*",
        "* -B*",
        "*--backup*",
        "* -V*",
        "*--version-control*",
        "* -Y*",
        "*--basename-prefix*",
        "* -z*",
        "*--suffix*",
        "*--prefix*",
        "* -r*",
        "*--reject-file*",
        "* -e*",
        "*--ed*",
        "* -- *",
        "* --",
    ];
    let mut out: Vec<String> = source_prefixes(workspace)
        .iter()
        .flat_map(|p| {
            options
                .iter()
                .map(move |pat| format!("Bash({p}patch {pat})"))
        })
        .collect();
    out.extend(path_and_metachar_denies(workspace, "patch"));
    out
}

/// The path and metacharacter denies of a granted `program` (quilt, `patch`),
/// for every prefix of [`source_prefixes`].
///
/// Tab and newline have no pattern of their own. The CLI reads the whitespace
/// of a rule as a separator, so `*<tab>*` is the rule `* *` and refuses every
/// command with two words, so `quilt refresh -p ab ...` and `quilt pop -a` are
/// refused. A tab in an argument is matched as a
/// space (`quilt add x<tab>/abs` is refused by `* /*`), and a newline between
/// commands is checked line by line (one refused line refuses the whole
/// command).
fn path_and_metachar_denies(workspace: &std::path::Path, program: &str) -> Vec<String> {
    let prefixes: Vec<String> = source_prefixes(workspace)
        .into_iter()
        .map(|p| format!("{p}{program}"))
        .collect();
    let patterns = [
        "* /*",
        "* \"/*",
        "* '/*",
        "*=/*",
        "* \"\"*",
        "* ''*",
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
        "*{*",
        "*}*",
        "*[*",
        "*]*",
        "*?*",
        "*!*",
        "*\\*",
    ];
    prefixes
        .iter()
        .flat_map(|p| patterns.iter().map(move |pat| format!("Bash({p} {pat})")))
        .collect()
}

/// Denies that win over the `Bash(rpm -qlp <rpms dir>/*)` grant. Its trailing
/// `*` matches any text, spaces included, so `rpm -qlp x.rpm --eval '%(...)'`
/// matches it: rpm takes `--eval`/`-E` and `--define`/`-D` after the file name,
/// expands `%(...)` by running a shell, and `--macros`, `--rcfile` and `--pipe`
/// load or run more. Each rule needs a space before the option, so a file name
/// containing the letters does not match. The metacharacter rules block
/// chaining or substitution inside the granted command; the engine also checks
/// each part of a compound command on its own.
fn rpm_denied_tools() -> Vec<String> {
    let options = [
        "--eval*",
        "--define*",
        "--macros*",
        "--rcfile*",
        "--pipe*",
        "-*E*",
        "-*D*",
    ];
    // `{`/`}` are brace expansion, `[`/`]`/`?` globs and `..` a parent path:
    // each turns the granted directory prefix into a path outside it. No tab
    // or newline entry: see `path_and_metachar_denies`.
    let metachars = [
        ";", "&", "|", "`", "$(", "%(", ">", "<", "{", "}", "[", "]", "?", "..",
    ];
    options
        .iter()
        .map(|o| format!("Bash(rpm * {o})"))
        .chain(metachars.iter().map(|m| format!("Bash(rpm *{m}*)")))
        .collect()
}
