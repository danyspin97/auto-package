//! The package lookup a turn runs through the `provides` sub-command: which
//! package provides a pkg-config module, a CMake package, a linker library or
//! a file. It runs `zypper search` against the repository data already on this
//! machine (no refresh, no root, no network; the host's repositories, which can
//! differ from the project's targets), with arguments built here from
//! validated words and never through a shell, so a name with parentheses
//! (`pkgconfig(xkbcommon)`) is asked without the turn spelling it.

use std::path::Path;
use std::time::Duration;

use crate::proc::{Emit, capture_with_timeout_env, minimal_env};

/// The longest a single zypper query may run.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// The most output bytes the lookup prints.
const MAX_OUTPUT: usize = 8 * 1024;

/// What is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A pkg-config module: `xkbcommon` asks for `pkgconfig(xkbcommon)`.
    Pkgconfig,
    /// A CMake package: `Qt5Core` asks for `cmake(Qt5Core)`.
    Cmake,
    /// A linker library, as in `-lpam`: the pkg-config module of that name.
    Lib,
    /// A file by its path without the leading `/`, such as `usr/bin/tool`.
    /// Only a path in package metadata (a tool) is found; ask headers and
    /// libraries through the pkg-config, CMake or library kinds.
    File,
}

impl Kind {
    /// The kind a command-line word names.
    pub fn parse(word: &str) -> Option<Kind> {
        match word {
            "pkgconfig" => Some(Kind::Pkgconfig),
            "cmake" => Some(Kind::Cmake),
            "lib" => Some(Kind::Lib),
            "file" => Some(Kind::File),
            _ => None,
        }
    }
}

/// A module, package or library name: letters, digits and `._+-`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// A path under `/` of letters, digits and `._+-/`, written without the
/// leading `/` (the turn's Bash rules refuse an absolute path) and with no `..`
/// element.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.len() <= 200
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-/".contains(c))
        && !path.split('/').any(|e| e == "..")
}

/// One zypper query: a label for the output and its arguments.
type Query = (String, Vec<String>);

fn zypper(extra: &[&str], what: String) -> Vec<String> {
    ["--non-interactive", "--no-refresh", "search"]
        .iter()
        .chain(extra)
        .map(|s| s.to_string())
        .chain([what])
        .collect()
}

fn provides_query(capability: String) -> Query {
    (
        capability.clone(),
        zypper(&["--provides", "--match-exact"], capability),
    )
}

/// The zypper queries for `kind` and `name`, or why the words are refused.
pub fn plan(kind: Kind, name: &str) -> Result<Vec<Query>, String> {
    let bad =
        |what: &str| format!("refused: {what} `{name}` has characters outside its allowed set");
    match kind {
        Kind::Pkgconfig | Kind::Cmake | Kind::Lib if !valid_name(name) => Err(bad("the name")),
        Kind::File if !valid_path(name) => Err(bad("the path")),
        Kind::Pkgconfig => Ok(vec![provides_query(format!("pkgconfig({name})"))]),
        Kind::Cmake => Ok(vec![provides_query(format!("cmake({name})"))]),
        Kind::Lib => {
            let lib = name.strip_prefix("lib").unwrap_or(name);
            Ok(vec![provides_query(format!("pkgconfig({lib})"))])
        }
        Kind::File => Ok(vec![provides_query(format!("/{name}"))]),
    }
}

/// The text of a lookup: each query's label and zypper's answer, capped.
/// `run` runs one query's arguments and gives its output (`None` for zypper's
/// "no matching items" exit) or an error line.
pub fn lookup_with(
    kind: Kind,
    name: &str,
    run: impl Fn(&[String]) -> Result<Option<String>, String>,
) -> Result<String, String> {
    let mut out = String::new();
    for (label, args) in plan(kind, name)? {
        out.push_str(&format!("query {label}:\n"));
        match run(&args) {
            Ok(Some(text)) => out.push_str(&answer(&text)),
            Ok(None) => out.push_str("  no match\n"),
            Err(e) => out.push_str(&format!(
                "  could not query: {e}\n  the lookup is unavailable: do not guess a package \
                 name, write `unverified`\n"
            )),
        }
    }
    if out.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[output cut]\n");
    }
    Ok(out)
}

/// The result rows of a zypper answer, without its progress lines.
fn answer(text: &str) -> String {
    let rows: Vec<&str> = text.lines().filter(|l| l.contains('|')).collect();
    if rows.is_empty() {
        return "  no answer\n".to_string();
    }
    rows.iter().map(|l| format!("  {l}\n")).collect()
}

/// The zypper to run: the system one when it exists, else a `PATH` lookup.
fn zypper_program() -> &'static str {
    if Path::new("/usr/bin/zypper").exists() {
        "/usr/bin/zypper"
    } else {
        "zypper"
    }
}

/// The environment of a query: the usual minimal one, with the C locale so the
/// output does not depend on the user's language.
fn query_env() -> std::collections::BTreeMap<String, String> {
    let mut env = minimal_env(&[]);
    env.insert("LC_ALL".into(), "C".into());
    env
}

/// [`lookup_with`] running the real zypper.
pub fn lookup(kind: Kind, name: &str) -> Result<String, String> {
    let env = query_env();
    lookup_with(kind, name, |args| {
        let got = capture_with_timeout_env(
            zypper_program(),
            args,
            Path::new("/"),
            Emit::quiet(),
            QUERY_TIMEOUT,
            Some(&env),
        )
        .map_err(|e| e.to_string())?;
        // Exit 104 is "no matching items": an answer, not a failure.
        if got.code == 0 {
            Ok(Some(got.stdout))
        } else if got.code == 104 {
            Ok(None)
        } else {
            Err(format!("zypper exited {}: {}", got.code, got.stderr.trim()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(kind: Kind, name: &str) -> Vec<String> {
        plan(kind, name).unwrap().remove(0).1
    }

    #[test]
    fn a_pkgconfig_module_asks_for_its_capability_with_fixed_flags() {
        assert_eq!(
            first(Kind::Pkgconfig, "xkbcommon"),
            [
                "--non-interactive",
                "--no-refresh",
                "search",
                "--provides",
                "--match-exact",
                "pkgconfig(xkbcommon)"
            ]
        );
        assert_eq!(
            first(Kind::Cmake, "Qt5Core").last().unwrap(),
            "cmake(Qt5Core)"
        );
    }

    #[test]
    fn a_library_asks_only_for_its_module() {
        for name in ["pam", "libpam"] {
            let q = plan(Kind::Lib, name).unwrap();
            assert_eq!(q.len(), 1);
            assert_eq!(q[0].1.last().unwrap(), "pkgconfig(pam)");
        }
    }

    #[test]
    fn a_file_asks_for_provides_with_an_exact_path_and_never_the_file_list() {
        let q = plan(Kind::File, "usr/include/foo.h").unwrap();
        assert_eq!(q.len(), 1);
        assert!(q[0].1.contains(&"--provides".to_string()));
        assert_eq!(q[0].1.last().unwrap(), "/usr/include/foo.h");
        for kind in [Kind::Pkgconfig, Kind::Cmake, Kind::Lib, Kind::File] {
            let name = if kind == Kind::File { "usr/bin/x" } else { "x" };
            for (_, args) in plan(kind, name).unwrap() {
                assert!(!args.contains(&"--file-list".to_string()), "{args:?}");
            }
        }
    }

    #[test]
    fn the_query_runs_in_the_c_locale_with_a_path() {
        let env = query_env();
        assert_eq!(env.get("LC_ALL").map(String::as_str), Some("C"));
        assert!(env.contains_key("PATH"));
    }

    #[test]
    fn zypper_is_the_system_one_when_it_exists() {
        let want = if Path::new("/usr/bin/zypper").exists() {
            "/usr/bin/zypper"
        } else {
            "zypper"
        };
        assert_eq!(zypper_program(), want);
    }

    #[test]
    fn no_match_comes_from_the_exit_code_not_from_localized_text() {
        let text = lookup_with(Kind::Pkgconfig, "x", |_| Ok(None)).unwrap();
        assert!(text.contains("no match"), "{text}");
        let text = lookup_with(Kind::Pkgconfig, "x", |_| {
            Ok(Some("Keine Treffer | gefunden\n".into()))
        })
        .unwrap();
        assert!(!text.contains("no match"), "{text}");
    }

    #[test]
    fn words_outside_the_allowed_set_are_refused() {
        for name in [
            "",
            "-x",
            "a b",
            "a;b",
            "a|b",
            "a$b",
            "a`b",
            "a(b)",
            "a'b",
            "a/b",
            "../x",
            "a\nb",
            "a&b",
            "a>b",
            "*",
            &"x".repeat(101),
        ] {
            for kind in [Kind::Pkgconfig, Kind::Cmake, Kind::Lib] {
                assert!(plan(kind, name).is_err(), "{kind:?} {name:?}");
            }
        }
        for path in [
            "/usr/x",
            "usr/../etc",
            "usr/a b",
            "usr/a;b",
            "a$b",
            "a`b",
            "a(b)",
            "*",
            "usr/x\n",
            "",
        ] {
            assert!(plan(Kind::File, path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn no_query_names_a_changing_subcommand() {
        let mut all = plan(Kind::Lib, "pam").unwrap();
        all.extend(plan(Kind::File, "usr/bin/x").unwrap());
        all.extend(plan(Kind::Cmake, "Qt5Core").unwrap());
        for (_, args) in all {
            assert_eq!(&args[..3], ["--non-interactive", "--no-refresh", "search"]);
            for bad in ["install", "remove", "addrepo", "refresh", "dup", "patch"] {
                assert!(!args[3..].contains(&bad.to_string()), "{args:?}");
            }
        }
    }

    #[test]
    fn the_output_keeps_result_rows_and_is_capped() {
        let text = lookup_with(Kind::Pkgconfig, "x", |_| {
            Ok(Some(
                "Loading...\nS | Name | Type\n  | foo-devel | package\n".into(),
            ))
        })
        .unwrap();
        assert!(
            text.contains("foo-devel") && !text.contains("Loading"),
            "{text}"
        );
        let long = lookup_with(Kind::Pkgconfig, "x", |_| {
            Ok(Some(format!("a | {}\n", "y".repeat(20000))))
        })
        .unwrap();
        assert!(long.len() < MAX_OUTPUT + 20 && long.ends_with("[output cut]\n"));
        let err = lookup_with(Kind::Pkgconfig, "x", |_| Err("boom".into())).unwrap();
        assert!(err.contains("could not query: boom"), "{err}");
        assert!(
            err.contains("do not guess") && err.contains("unverified"),
            "{err}"
        );
    }
}
