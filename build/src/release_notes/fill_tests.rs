//! The fill of a missing recap. The changelog is a made-up fixture shaped like
//! the python-ConfigArgParse one (origin 1.7.5, branch 1.9.0); the recap of
//! every case comes from the fixture, never from the network.

use super::changelog::section_between;
use super::fill::fill_with;
use super::per_release::{from_section, several};
use super::*;
use std::cell::Cell;
use std::path::Path;

const NEWS: &str = "\
## 1.9.0

- Add a new option
- Fix the parser on Python 3.13

## 1.8.0

- Support TOML files
- Fix the help text

## 1.7.5

- Older change
";

const SEP: &str = "-------------------------------------------------------------------";

const OLD_ENTRY: &str = "Mon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.7.5\n";

fn spec(version: &str) -> String {
    format!("Name: python-ConfigArgParse\nVersion: {version}\nRelease: 0\nSummary: x\n")
}

fn origin_spec() -> String {
    spec("1.7.5")
}

/// The `.changes` of the branch: a new entry `body` above the published one.
fn changes(body: &str) -> String {
    format!("{SEP}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n{SEP}\n{OLD_ENTRY}")
}

fn origin_changes() -> String {
    format!("{SEP}\n{OLD_ENTRY}")
}

fn no_model(_: &str) -> Option<String> {
    None
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

/// A recap that reads the fixture's sections between `old` and `new`, as the
/// changelog of the sources does (one group per release, newest first).
fn changelog_of(
    news: &'static str,
) -> impl Fn(Option<&str>, Option<&Path>, &str, &str, bool) -> Recap {
    move |_, _, old, new, _| {
        let found = section_between(news, old, new)
            .map(|s| from_section("NEWS", &s, old, new))
            .unwrap_or_default();
        let releases = several(new, old, found, Vec::new(), no_model).unwrap_or_default();
        Recap {
            releases,
            fetch_problem: None,
        }
    }
}

/// A checkout with `<pkg>.spec` at `version` and `<pkg>.changes` as `text`.
fn checkout(version: &str, text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("python-ConfigArgParse.spec"), spec(version)).unwrap();
    std::fs::write(dir.path().join("python-ConfigArgParse.changes"), text).unwrap();
    dir
}

fn read(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("python-ConfigArgParse.changes")).unwrap()
}

fn fill(
    dir: &tempfile::TempDir,
    recap: &dyn Fn(Option<&str>, Option<&Path>, &str, &str, bool) -> Recap,
    no_agent: bool,
) -> Result<Fill, String> {
    fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &origin_changes(),
        &origin_spec(),
        no_agent,
        recap,
    )
}

#[test]
fn a_bare_bullet_gets_one_bullet_per_release_newest_first() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let recap = changelog_of(NEWS);
    let got = fill(&dir, &recap, true).unwrap();
    assert_eq!(
        got,
        Fill::Written {
            old: "1.7.5".into(),
            new: "1.9.0".into()
        }
    );
    let out = read(&dir);
    let own = out.find("- Update to 1.9.0:\n  * Add a new option");
    let older = out.find("- Update to 1.8.0:\n  * Support TOML files");
    let (own, older) = (
        own.unwrap_or_else(|| panic!("{out}")),
        older.unwrap_or_else(|| panic!("{out}")),
    );
    assert!(own < older, "{out}");
    assert!(out.ends_with(&format!("{OLD_ENTRY}")), "{out}");
}

#[test]
fn a_single_release_gets_its_notes_in_todays_form() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0\n- Fix the build"));
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: vec![("1.9.0".to_string(), lines(&["* Add a new option"]))],
        fetch_problem: None,
    };
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let out = read(&dir);
    assert!(
        out.contains("- Update to 1.9.0\n  * Add a new option\n- Fix the build\n"),
        "{out}"
    );
    assert!(!out.contains("Update to 1.9.0:"), "{out}");
}

#[test]
fn an_entry_whose_bullet_has_notes_is_untouched() {
    let text = changes("- Update to 1.9.0\n  * Already here\n- Fix the build");
    let dir = checkout("1.9.0", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("the recap is not asked for an entry with notes")
    };
    assert_eq!(fill(&dir, &never, false), Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn a_flag_line_under_the_bullet_is_not_a_note() {
    let dir = checkout(
        "1.9.0",
        &changes("- Update to 1.9.0\n  FLAG: tests skipped: -k x"),
    );
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: vec![("1.9.0".to_string(), lines(&["* a"]))],
        fetch_problem: None,
    };
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let out = read(&dir);
    assert!(
        out.contains("- Update to 1.9.0\n  * a\n  FLAG: tests skipped: -k x\n"),
        "{out}"
    );
}

#[test]
fn no_notes_found_leaves_the_entry_and_says_why() {
    let text = changes("- Update to 1.9.0");
    let dir = checkout("1.9.0", &text);
    let none = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| Recap {
        releases: Vec::new(),
        fetch_problem: Some("could not list the releases of x".into()),
    };
    assert_eq!(
        fill(&dir, &none, true),
        Ok(Fill::NotFound {
            old: "1.7.5".into(),
            new: "1.9.0".into(),
            problem: Some("could not list the releases of x".into()),
        })
    );
    assert_eq!(read(&dir), text, "no placeholder is written");
}

#[test]
fn no_agent_is_passed_on_to_the_recap() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let seen = Cell::new(None);
    let recap = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, no_agent: bool| {
        seen.set(Some(no_agent));
        Recap::default()
    };
    let _ = fill(&dir, &recap, true);
    assert_eq!(seen.get(), Some(true));
}

#[test]
fn a_second_run_changes_nothing() {
    let dir = checkout("1.9.0", &changes("- Update to 1.9.0"));
    let recap = changelog_of(NEWS);
    assert!(matches!(fill(&dir, &recap, true), Ok(Fill::Written { .. })));
    let once = read(&dir);
    assert_eq!(fill(&dir, &recap, true), Ok(Fill::Nothing));
    assert_eq!(read(&dir), once);
}

#[test]
fn a_published_entry_is_never_touched() {
    let text = changes("- Update to 1.9.0");
    let dir = checkout("1.9.0", &text);
    // The origin publishes the same entry.
    let published = format!("{SEP}\n{}", text.split_once(&format!("{SEP}\n")).unwrap().1);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("a published entry is not filled")
    };
    let got = fill_with(
        dir.path(),
        "python-ConfigArgParse",
        &published,
        &origin_spec(),
        true,
        &never,
    );
    assert_eq!(got, Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn an_entry_for_another_version_is_not_filled() {
    let text = changes("- Update to 1.8.0");
    let dir = checkout("1.9.0", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("only the spec's version is filled")
    };
    assert_eq!(fill(&dir, &never, true), Ok(Fill::Nothing));
    assert_eq!(read(&dir), text);
}

#[test]
fn a_spec_at_the_origins_version_is_not_filled() {
    let text = changes("- Update to 1.7.5");
    let dir = checkout("1.7.5", &text);
    let never = |_: Option<&str>, _: Option<&Path>, _: &str, _: &str, _: bool| -> Recap {
        panic!("no update, no recap")
    };
    assert_eq!(fill(&dir, &never, true), Ok(Fill::Nothing));
}
